//! 文書モデル（ブロック検出と読み順）

use serde::Serialize;

use crate::columns::{Participant, ParticipantKind, split_regions};
use crate::error::{Error, Result};
use crate::extract::ExtractOptions;
use crate::furniture;
use crate::model::{BBox, Cell, Table};
use crate::reader::{extract_from_bytes, extract_text_from_bytes};
use crate::text::{
    TextChar, TextFont, TextLine, TextPage, TextWord, build_text_lines, derotate_xy,
    is_whitespace_str,
};

/// 段落併合の行送り上限（代表 font_size 比）
const PARA_LEADING_FACTOR: f64 = 1.8;
/// 段落併合の代表 font_size 比の上限（大きい方 / 小さい方）
const PARA_FS_RATIO_MAX: f64 = 1.33;
/// 見出し level 1 の本文代表サイズ比（§4-5）
const HEADING_L1_FACTOR: f64 = 1.8;
/// 見出し level 2 の本文代表サイズ比（§4-5）
const HEADING_L2_FACTOR: f64 = 1.4;
/// 見出し level 3 の本文代表サイズ比（§4-5）
const HEADING_L3_FACTOR: f64 = 1.2;
/// 本文代表サイズの丸め単位（pt）
const BODY_FS_BIN: f64 = 0.1;

/// 文書全体
#[derive(Debug, Clone, Serialize)]
pub struct DocDoc {
    pub pages: Vec<DocPage>,
}

/// 1ページ分の文書モデル
#[derive(Debug, Clone, Serialize)]
pub struct DocPage {
    pub page_number: usize,
    pub width: f64,
    pub height: f64,
    pub fonts: Vec<TextFont>,
    pub chars: Vec<TextChar>,
    pub blocks: Vec<DocBlock>,
}

/// ページ内の1ブロック
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum DocBlock {
    #[serde(rename = "table")]
    Table(Table),
    #[serde(rename = "text")]
    Text(TextBlock),
}

/// 本文ブロック
#[derive(Debug, Clone, Serialize)]
pub struct TextBlock {
    #[serde(flatten)]
    pub kind: TextBlockKind,
    #[serde(default, skip_serializing_if = "TextBlockRole::is_body")]
    pub role: TextBlockRole,
    pub text: String,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub dir: String,
    pub rot: i32,
    pub lines: Vec<TextLine>,
}

/// 本文ブロックの種別
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum TextBlockKind {
    #[serde(rename = "paragraph")]
    Paragraph,
    #[serde(rename = "heading")]
    Heading { level: u8 },
}

/// 本文ブロックの役割（ヘッダ・フッタ検出）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TextBlockRole {
    #[default]
    Body,
    Header,
    Footer,
}

impl TextBlockRole {
    fn is_body(&self) -> bool {
        *self == TextBlockRole::Body
    }
}

/// 文書単位でヘッダ・フッタの役割を付ける
pub fn assign_header_footer(doc: &mut DocDoc) {
    furniture::assign_header_footer(doc);
}

/// 文書単位で見出しを推定する（§4-5）
pub fn assign_headings(doc: &mut DocDoc) {
    let Some(body_fs) = body_rep_font_size(doc) else {
        return;
    };

    for page in &mut doc.pages {
        for block in &mut page.blocks {
            let DocBlock::Text(tb) = block else {
                continue;
            };
            if tb.role != TextBlockRole::Body {
                continue;
            }
            if tb.kind != TextBlockKind::Paragraph {
                continue;
            }
            if tb.lines.len() > 2 {
                continue;
            }
            let Some(block_fs) = block_rep_font_size(&page.chars, tb) else {
                continue;
            };
            let level = if block_fs >= body_fs * HEADING_L1_FACTOR {
                1
            } else if block_fs >= body_fs * HEADING_L2_FACTOR {
                2
            } else if block_fs >= body_fs * HEADING_L3_FACTOR {
                3
            } else {
                continue;
            };
            tb.kind = TextBlockKind::Heading { level };
        }
    }
}

/// 文書モデルを Markdown 文字列へ直列化する（§6）
pub fn document_to_markdown(doc: &DocDoc) -> String {
    let mut parts = Vec::new();
    for page in &doc.pages {
        for block in &page.blocks {
            if let Some(s) = block_to_markdown(block) {
                parts.push(s);
            }
        }
    }
    parts.join("\n\n")
}

/// 1ブロックを Markdown 断片へ変換する。空表・空白のみ本文は None
fn block_to_markdown(block: &DocBlock) -> Option<String> {
    match block {
        DocBlock::Text(tb) => {
            if tb.role != TextBlockRole::Body {
                return None;
            }
            let text = sanitize_md_text(&tb.text);
            if text.trim().is_empty() {
                return None;
            }
            match &tb.kind {
                TextBlockKind::Heading { level } => {
                    Some(format!("{} {}", "#".repeat(*level as usize), text))
                }
                TextBlockKind::Paragraph => Some(text),
            }
        }
        DocBlock::Table(table) => table_to_markdown(table),
    }
}

/// GFM パイプテーブルへ直列化する（§6-3）
fn table_to_markdown(table: &Table) -> Option<String> {
    if table.n_rows == 0 || table.n_cols == 0 {
        return None;
    }
    let n_cols = table.n_cols;
    let mut lines = Vec::with_capacity(table.data.len().max(1) + 1);

    let header = table.data.first().map(|r| r.as_slice()).unwrap_or(&[]);
    lines.push(format_table_row(header, n_cols));

    let mut sep = String::from("|");
    for _ in 0..n_cols {
        sep.push_str(" --- |");
    }
    lines.push(sep);

    for row in table.data.iter().skip(1) {
        lines.push(format_table_row(row, n_cols));
    }

    Some(lines.join("\n"))
}

/// 表の1行をパイプ区切りで組み立てる
fn format_table_row(row: &[Cell], n_cols: usize) -> String {
    let mut s = String::from("|");
    for i in 0..n_cols {
        s.push(' ');
        if let Some(cell) = row.get(i) {
            s.push_str(&escape_cell_text(&cell.text));
        }
        s.push_str(" |");
    }
    s
}

/// セル text の制御文字置換と GFM エスケープ（§6-4）
fn escape_cell_text(text: &str) -> String {
    let sanitized = sanitize_md_text(text);
    let mut out = String::with_capacity(sanitized.len());
    for c in sanitized.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            _ => out.push(c),
        }
    }
    out
}

/// 改行・タブ・C0 制御文字を U+0020 へ置換する（§6-4）
pub(crate) fn sanitize_md_text(text: &str) -> String {
    text.chars()
        .map(|c| {
            if is_md_control_char(c) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Markdown 直列化で空白へ置換する制御文字か
fn is_md_control_char(c: char) -> bool {
    matches!(c, '\u{0000}'..='\u{001F}' | '\u{2028}' | '\u{2029}')
}

/// PDF バイト列から見出し推定済みの文書モデルを構築する（§1・§4-1）
pub fn extract_document_from_bytes(
    data: &[u8],
    password: Option<&str>,
    options: &ExtractOptions,
) -> Result<DocDoc> {
    let tables_doc = extract_from_bytes(data, password, options)?;
    let text_doc = extract_text_from_bytes(data, password)?;
    if tables_doc.pages.len() != text_doc.pages.len() {
        return Err(Error::Reader(format!(
            "page count mismatch: tables has {} pages, text has {} pages",
            tables_doc.pages.len(),
            text_doc.pages.len()
        )));
    }

    let mut pages = Vec::with_capacity(text_doc.pages.len());
    for (i, (page, text_page)) in tables_doc
        .pages
        .into_iter()
        .zip(text_doc.pages.into_iter())
        .enumerate()
    {
        let blocks = build_blocks_owned(&text_page, page.tables);
        pages.push(DocPage {
            page_number: i + 1,
            width: text_page.width,
            height: text_page.height,
            fonts: text_page.fonts,
            chars: text_page.chars,
            blocks,
        });
    }

    let mut doc = DocDoc { pages };
    if options.detect_header_footer {
        assign_header_footer(&mut doc);
    }
    assign_headings(&mut doc);
    Ok(doc)
}

/// 本文代表 font_size（0.1pt 丸め最頻値。同数タイは小さい方）
fn body_rep_font_size(doc: &DocDoc) -> Option<f64> {
    let mut counts: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    for page in &doc.pages {
        for block in &page.blocks {
            let DocBlock::Text(tb) = block else {
                continue;
            };
            if tb.role != TextBlockRole::Body {
                continue;
            }
            for line in &tb.lines {
                for &idx in &line.chars {
                    let ch = &page.chars[idx as usize];
                    if is_whitespace_str(&ch.text) {
                        continue;
                    }
                    let fs = ch.font_size;
                    if fs.is_finite() && fs > 0.0 {
                        let key = (fs / BODY_FS_BIN).round() as i64;
                        *counts.entry(key).or_insert(0) += 1;
                    }
                }
            }
        }
    }
    if counts.is_empty() {
        return None;
    }
    let mut best_key = i64::MAX;
    let mut best_count = 0usize;
    for (&key, &count) in &counts {
        if count > best_count || (count == best_count && key < best_key) {
            best_count = count;
            best_key = key;
        }
    }
    Some(best_key as f64 * BODY_FS_BIN)
}

/// ブロック構成行の代表 font_size の中央値
pub(crate) fn block_rep_font_size(chars: &[TextChar], tb: &TextBlock) -> Option<f64> {
    let mut vals = Vec::with_capacity(tb.lines.len());
    for line in &tb.lines {
        let fs = line_rep_font_size(chars, line)?;
        vals.push(fs);
    }
    median_f64(&mut vals)
}

/// ページの行と表から読み順付きブロック列を構築する
pub fn build_blocks(page: &TextPage, tables: &[Table]) -> Vec<DocBlock> {
    build_blocks_owned(page, tables.to_vec())
}

/// 表を所有権ごと受け取りブロック列を構築する
fn build_blocks_owned(page: &TextPage, tables: Vec<Table>) -> Vec<DocBlock> {
    let valid_cells = collect_valid_cells(&tables);
    let raw_lines = build_text_lines(page);
    let body_lines: Vec<TextLine> = raw_lines
        .into_iter()
        .filter_map(|line| filter_table_owned_line(page, line, &valid_cells))
        .collect();

    let (parts, word_part_ids, table_part_ids, other_part_ids) =
        collect_participants(&body_lines, &tables);

    if let Some(leaf_of) = split_regions(page.width, page.height, &parts) {
        return build_blocks_with_regions(
            page,
            body_lines,
            tables,
            &leaf_of,
            &word_part_ids,
            &table_part_ids,
            &other_part_ids,
        );
    }

    let text_blocks = build_text_blocks(page, body_lines);

    let n_tables = tables.len();
    let mut keyed: Vec<(f64, f64, usize, DocBlock)> =
        Vec::with_capacity(n_tables + text_blocks.len());
    for (i, table) in tables.into_iter().enumerate() {
        let top = table.bbox.top;
        let left = table.bbox.x0;
        keyed.push((top, left, i, DocBlock::Table(table)));
    }
    let mut gen_no = n_tables;
    for tb in text_blocks {
        let top = tb.top;
        let left = tb.left;
        keyed.push((top, left, gen_no, DocBlock::Text(tb)));
        gen_no += 1;
    }

    keyed.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| a.1.total_cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    keyed.into_iter().map(|(_, _, _, b)| b).collect()
}

/// 対象本文行か（横書き・正立）
fn is_target_body_line(line: &TextLine) -> bool {
    line.dir == "ltr" && line.rot == 0
}

/// 参加要素列を安定ID順で構築する
fn collect_participants(
    body_lines: &[TextLine],
    tables: &[Table],
) -> (
    Vec<Participant>,
    Vec<Vec<usize>>,
    Vec<usize>,
    Vec<Option<usize>>,
) {
    let mut parts = Vec::new();
    let mut word_part_ids: Vec<Vec<usize>> = vec![Vec::new(); body_lines.len()];
    let mut other_part_ids: Vec<Option<usize>> = vec![None; body_lines.len()];
    let mut table_part_ids = Vec::with_capacity(tables.len());

    for (li, line) in body_lines.iter().enumerate() {
        if !is_target_body_line(line) {
            continue;
        }
        for word in &line.words {
            let id = parts.len();
            word_part_ids[li].push(id);
            parts.push(Participant {
                left: word.left,
                right: word.right,
                top: word.top,
                bottom: word.bottom,
                kind: ParticipantKind::Word { line: li },
            });
        }
    }
    for table in tables {
        let id = parts.len();
        table_part_ids.push(id);
        parts.push(Participant {
            left: table.bbox.x0,
            right: table.bbox.x1,
            top: table.bbox.top,
            bottom: table.bbox.bottom,
            kind: ParticipantKind::Table,
        });
    }
    for (li, line) in body_lines.iter().enumerate() {
        if is_target_body_line(line) {
            continue;
        }
        let id = parts.len();
        other_part_ids[li] = Some(id);
        parts.push(Participant {
            left: line.left,
            right: line.right,
            top: line.top,
            bottom: line.bottom,
            kind: ParticipantKind::OtherLine,
        });
    }

    (parts, word_part_ids, table_part_ids, other_part_ids)
}

/// X分割ありのページで葉領域ごとにブロック列を構築する
fn build_blocks_with_regions(
    page: &TextPage,
    body_lines: Vec<TextLine>,
    tables: Vec<Table>,
    leaf_of: &[usize],
    word_part_ids: &[Vec<usize>],
    table_part_ids: &[usize],
    other_part_ids: &[Option<usize>],
) -> Vec<DocBlock> {
    let n_leaves = leaf_of.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    let mut leaf_lines: Vec<Vec<TextLine>> = (0..n_leaves).map(|_| Vec::new()).collect();

    for (li, line) in body_lines.into_iter().enumerate() {
        if is_target_body_line(&line) {
            let word_leaves: Vec<usize> = word_part_ids[li]
                .iter()
                .map(|&pid| leaf_of[pid])
                .collect();
            for (leaf, frag) in reconstitute_line_by_leaves(page, line, &word_leaves) {
                if leaf < leaf_lines.len() {
                    leaf_lines[leaf].push(frag);
                }
            }
        } else if let Some(pid) = other_part_ids[li] {
            let leaf = leaf_of[pid];
            if leaf < leaf_lines.len() {
                leaf_lines[leaf].push(line);
            }
        }
    }

    let n_tables = tables.len();
    let mut keyed: Vec<(usize, f64, f64, usize, DocBlock)> = Vec::new();

    for (i, table) in tables.into_iter().enumerate() {
        let leaf = leaf_of[table_part_ids[i]];
        let top = table.bbox.top;
        let left = table.bbox.x0;
        keyed.push((leaf, top, left, i, DocBlock::Table(table)));
    }

    let mut gen_no = n_tables;
    for (leaf, lines) in leaf_lines.into_iter().enumerate() {
        if lines.is_empty() {
            continue;
        }
        let text_blocks = build_text_blocks(page, lines);
        for tb in text_blocks {
            let top = tb.top;
            let left = tb.left;
            keyed.push((leaf, top, left, gen_no, DocBlock::Text(tb)));
            gen_no += 1;
        }
    }

    keyed.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.total_cmp(&b.1))
            .then_with(|| a.2.total_cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    keyed.into_iter().map(|(_, _, _, _, b)| b).collect()
}

/// 対象本文行を葉ごとの行断片へ振り分ける
fn reconstitute_line_by_leaves(
    page: &TextPage,
    line: TextLine,
    word_leaves: &[usize],
) -> Vec<(usize, TextLine)> {
    if line.words.is_empty() || word_leaves.is_empty() {
        return Vec::new();
    }
    debug_assert_eq!(line.words.len(), word_leaves.len());

    let first = word_leaves[0];
    if word_leaves.iter().all(|&l| l == first) {
        return vec![(first, line)];
    }

    // 各グリフが属する単語（無ければ単語外）
    let mut char_word: Vec<(u32, usize)> = Vec::new();
    for (wi, w) in line.words.iter().enumerate() {
        for &c in &w.chars {
            char_word.push((c, wi));
        }
    }
    char_word.sort_unstable_by_key(|&(c, _)| c);

    let find_word = |cidx: u32| -> Option<usize> {
        char_word
            .binary_search_by_key(&cidx, |&(c, _)| c)
            .ok()
            .map(|i| char_word[i].1)
    };

    // 葉の出現順（語順）
    let mut leaf_order: Vec<usize> = Vec::new();
    for &l in word_leaves {
        if !leaf_order.contains(&l) {
            leaf_order.push(l);
        }
    }

    // 単語外空白の葉（進行軸順で直前語、行頭は直後語）
    let mut free_ws_leaf: Vec<(u32, usize)> = Vec::new();
    let mut last_word: Option<usize> = None;
    for (pos, &cidx) in line.chars.iter().enumerate() {
        if let Some(wi) = find_word(cidx) {
            last_word = Some(wi);
            continue;
        }
        if !is_whitespace_str(&page.chars[cidx as usize].text) {
            continue;
        }
        if let Some(wi) = last_word {
            free_ws_leaf.push((cidx, word_leaves[wi]));
        } else {
            let mut next = None;
            for &c2 in &line.chars[pos + 1..] {
                if let Some(wi) = find_word(c2) {
                    next = Some(wi);
                    break;
                }
            }
            if let Some(wi) = next {
                free_ws_leaf.push((cidx, word_leaves[wi]));
            }
        }
    }
    free_ws_leaf.sort_unstable_by_key(|&(c, _)| c);

    let free_leaf = |cidx: u32| -> Option<usize> {
        free_ws_leaf
            .binary_search_by_key(&cidx, |&(c, _)| c)
            .ok()
            .map(|i| free_ws_leaf[i].1)
    };

    // 元の line.chars 順を保って各葉へ積む
    let mut chars_by_leaf: Vec<Vec<u32>> = vec![Vec::new(); leaf_order.len()];
    let leaf_slot = |leaf: usize| -> Option<usize> {
        leaf_order.iter().position(|&l| l == leaf)
    };
    for &cidx in &line.chars {
        let leaf = if let Some(wi) = find_word(cidx) {
            word_leaves[wi]
        } else if let Some(l) = free_leaf(cidx) {
            l
        } else {
            continue;
        };
        if let Some(slot) = leaf_slot(leaf) {
            chars_by_leaf[slot].push(cidx);
        }
    }

    let mut out = Vec::new();
    for (slot, &leaf) in leaf_order.iter().enumerate() {
        let words: Vec<TextWord> = line
            .words
            .iter()
            .zip(word_leaves.iter())
            .filter(|(_, l)| **l == leaf)
            .map(|(w, _)| w.clone())
            .collect();
        if words.is_empty() {
            continue;
        }
        let chars = chars_by_leaf[slot].clone();
        let Some((left, right, top, bottom)) = non_ws_union_bbox(page, &chars) else {
            continue;
        };
        out.push((
            leaf,
            TextLine {
                left,
                right,
                top,
                bottom,
                dir: line.dir.clone(),
                rot: line.rot,
                words,
                chars,
            },
        ));
    }
    out
}

/// 有効セルの bbox を集める
fn collect_valid_cells(tables: &[Table]) -> Vec<BBox> {
    let mut out = Vec::new();
    for table in tables {
        for cell in table.data.iter().flatten() {
            if cell.text.is_empty() {
                continue;
            }
            if cell.bbox.width() <= 0.0 || cell.bbox.height() <= 0.0 {
                continue;
            }
            out.push(cell.bbox);
        }
    }
    out
}

/// 表所有グリフを除いた行を返す
fn filter_table_owned_line(
    page: &TextPage,
    line: TextLine,
    cells: &[BBox],
) -> Option<TextLine> {
    if cells.is_empty() {
        return Some(line);
    }

    let mut any_owned = false;
    let mut any_free_non_ws = false;
    let mut owned_idx = std::collections::HashSet::new();
    for &idx in &line.chars {
        let ch = &page.chars[idx as usize];
        let owned = glyph_table_owned(ch, cells);
        if owned {
            any_owned = true;
            owned_idx.insert(idx);
        } else if !is_whitespace_str(&ch.text) {
            any_free_non_ws = true;
        }
    }
    if !any_free_non_ws {
        return None;
    }
    if !any_owned {
        return Some(line);
    }

    let new_chars: Vec<u32> = line
        .chars
        .into_iter()
        .filter(|idx| !owned_idx.contains(idx))
        .collect();

    let mut new_words = Vec::new();
    for word in line.words {
        let wchars: Vec<u32> = word
            .chars
            .into_iter()
            .filter(|idx| !owned_idx.contains(idx))
            .collect();
        if let Some(w) = remake_word(page, &wchars) {
            new_words.push(w);
        }
    }
    if new_words.is_empty() {
        return None;
    }

    let (left, right, top, bottom) = non_ws_union_bbox(page, &new_chars)?;
    Some(TextLine {
        left,
        right,
        top,
        bottom,
        dir: line.dir,
        rot: line.rot,
        words: new_words,
        chars: new_chars,
    })
}

/// グリフ中心が有効セルに含まれるか
fn glyph_table_owned(ch: &TextChar, cells: &[BBox]) -> bool {
    let cx = (ch.left + ch.right) / 2.0;
    let cy = (ch.top + ch.bottom) / 2.0;
    cells.iter().any(|c| c.contains_point(cx, cy))
}

/// 残存グリフから単語を再構成する
fn remake_word(page: &TextPage, chars: &[u32]) -> Option<TextWord> {
    if chars.is_empty() {
        return None;
    }
    let mut left = f64::INFINITY;
    let mut right = f64::NEG_INFINITY;
    let mut top = f64::INFINITY;
    let mut bottom = f64::NEG_INFINITY;
    let mut any = false;
    let mut text = String::new();
    let mut out_chars = Vec::with_capacity(chars.len());
    for &i in chars {
        let ch = &page.chars[i as usize];
        out_chars.push(i);
        text.push_str(&ch.text);
        if is_whitespace_str(&ch.text) {
            continue;
        }
        any = true;
        left = left.min(ch.left);
        right = right.max(ch.right);
        top = top.min(ch.top);
        bottom = bottom.max(ch.bottom);
    }
    if !any {
        return None;
    }
    Some(TextWord {
        text,
        left,
        right,
        top,
        bottom,
        chars: out_chars,
    })
}

/// 非空白グリフ bbox の和集合
fn non_ws_union_bbox(page: &TextPage, chars: &[u32]) -> Option<(f64, f64, f64, f64)> {
    let mut left = f64::INFINITY;
    let mut right = f64::NEG_INFINITY;
    let mut top = f64::INFINITY;
    let mut bottom = f64::NEG_INFINITY;
    let mut any = false;
    for &i in chars {
        let ch = &page.chars[i as usize];
        if is_whitespace_str(&ch.text) {
            continue;
        }
        any = true;
        left = left.min(ch.left);
        right = right.max(ch.right);
        top = top.min(ch.top);
        bottom = bottom.max(ch.bottom);
    }
    if any {
        Some((left, right, top, bottom))
    } else {
        None
    }
}

/// 行の代表値（ソート・併合で使い回す）
struct LineRep {
    baseline: f64,
    font_size: Option<f64>,
    progress: (f64, f64),
    stream_index: u32,
}

/// 本文行から段落ブロックを生成する
fn build_text_blocks(page: &TextPage, lines: Vec<TextLine>) -> Vec<TextBlock> {
    if lines.is_empty() {
        return Vec::new();
    }

    // 書字方向グループ (dir, rot) ごとに最小 chars インデックスを求める
    let mut group_keys: Vec<(String, i32)> = Vec::new();
    let mut group_lines: Vec<Vec<(LineRep, TextLine)>> = Vec::new();
    let mut group_min: Vec<u32> = Vec::new();
    let mut key_pos: std::collections::HashMap<(String, i32), usize> =
        std::collections::HashMap::new();

    for line in lines {
        let key = (line.dir.clone(), line.rot);
        let rep = LineRep {
            baseline: line_rep_baseline(page, &line),
            font_size: line_rep_font_size(&page.chars, &line),
            progress: line_progress_interval(page, &line),
            stream_index: line_stream_index(&line),
        };
        let min_idx = rep.stream_index;
        if let Some(&pos) = key_pos.get(&key) {
            group_min[pos] = group_min[pos].min(min_idx);
            group_lines[pos].push((rep, line));
        } else {
            let pos = group_keys.len();
            key_pos.insert(key.clone(), pos);
            group_keys.push(key);
            group_min.push(min_idx);
            group_lines.push(vec![(rep, line)]);
        }
    }

    let mut order: Vec<usize> = (0..group_keys.len()).collect();
    order.sort_by(|&a, &b| {
        group_min[a]
            .cmp(&group_min[b])
            .then_with(|| a.cmp(&b))
    });

    let mut blocks = Vec::new();
    for gi in order {
        let mut glines = std::mem::take(&mut group_lines[gi]);
        glines.sort_by(|a, b| {
            a.0.baseline
                .total_cmp(&b.0.baseline)
                .then_with(|| a.0.stream_index.cmp(&b.0.stream_index))
        });

        let mut para: Vec<TextLine> = Vec::new();
        let mut prev_rep: Option<LineRep> = None;
        for (rep, line) in glines {
            if para.is_empty() {
                para.push(line);
                prev_rep = Some(rep);
                continue;
            }
            let prev = prev_rep.as_ref().unwrap();
            if can_merge_line_reps(prev, &rep) {
                para.push(line);
                prev_rep = Some(rep);
            } else {
                blocks.push(make_text_block(std::mem::take(&mut para)));
                para.push(line);
                prev_rep = Some(rep);
            }
        }
        if !para.is_empty() {
            blocks.push(make_text_block(para));
        }
    }
    blocks
}

/// 隣接2行を1段落に併合できるか
fn can_merge_line_reps(a: &LineRep, b: &LineRep) -> bool {
    let Some(fs_a) = a.font_size else {
        return false;
    };
    let Some(fs_b) = b.font_size else {
        return false;
    };
    let gap = (b.baseline - a.baseline).abs();
    if gap > fs_a.max(fs_b) * PARA_LEADING_FACTOR {
        return false;
    }
    if !intervals_overlap(a.progress, b.progress) {
        return false;
    }
    let lo = fs_a.min(fs_b);
    let hi = fs_a.max(fs_b);
    if lo <= 0.0 {
        return false;
    }
    let ratio = hi / lo;
    ratio <= PARA_FS_RATIO_MAX
}

/// 行群から TextBlock を組み立てる
fn make_text_block(lines: Vec<TextLine>) -> TextBlock {
    let dir = lines[0].dir.clone();
    let rot = lines[0].rot;
    let text = join_paragraph_text(&lines);
    let mut left = f64::INFINITY;
    let mut right = f64::NEG_INFINITY;
    let mut top = f64::INFINITY;
    let mut bottom = f64::NEG_INFINITY;
    for line in &lines {
        left = left.min(line.left);
        right = right.max(line.right);
        top = top.min(line.top);
        bottom = bottom.max(line.bottom);
    }
    TextBlock {
        kind: TextBlockKind::Paragraph,
        role: TextBlockRole::Body,
        text,
        left,
        right,
        top,
        bottom,
        dir,
        rot,
        lines,
    }
}

/// 段落内の行を結合して本文文字列を作る
fn join_paragraph_text(lines: &[TextLine]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = join_line_words(&lines[0]);
    for i in 1..lines.len() {
        let prev = &lines[i - 1];
        let next = &lines[i];
        let next_str = join_line_words(next);
        let prev_last = prev.words.last().and_then(|w| w.text.chars().last());
        let next_first = next.words.first().and_then(|w| w.text.chars().next());
        match (prev_last, next_first) {
            (Some('\u{00AD}'), _) => {
                if out.ends_with('\u{00AD}') {
                    out.pop();
                }
                out.push_str(&next_str);
            }
            (Some('-'), Some(nf)) => {
                let before = prev
                    .words
                    .last()
                    .and_then(|w| {
                        let mut it = w.text.chars().rev();
                        let _hyphen = it.next();
                        it.next()
                    });
                if before.is_some_and(is_latin_letter)
                    && is_latin_letter(nf)
                    && nf.is_lowercase()
                {
                    if out.ends_with('-') {
                        out.pop();
                    }
                    out.push_str(&next_str);
                } else {
                    out.push_str(&next_str);
                }
            }
            (Some(pl), Some(nf)) if is_cjk(pl) || is_cjk(nf) => {
                out.push_str(&next_str);
            }
            (Some(_), Some(_)) => {
                out.push(' ');
                out.push_str(&next_str);
            }
            _ => {
                if !next_str.is_empty() {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&next_str);
                }
            }
        }
    }
    out
}

/// 行内単語を空白で連結する
fn join_line_words(line: &TextLine) -> String {
    let mut s = String::new();
    for (i, w) in line.words.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&w.text);
    }
    s
}

/// 行のストリーム順キー
fn line_stream_index(line: &TextLine) -> u32 {
    line.chars.iter().copied().min().unwrap_or(u32::MAX)
}

/// 行の代表ベースライン
fn line_rep_baseline(page: &TextPage, line: &TextLine) -> f64 {
    let vertical = line.dir == "ttb";
    let mut vals = Vec::new();
    for &idx in &line.chars {
        let ch = &page.chars[idx as usize];
        if is_whitespace_str(&ch.text) {
            continue;
        }
        let (px, py) = derotate_xy(ch.transform[4], ch.transform[5], line.rot);
        let v = if vertical { px } else { py };
        if v.is_finite() {
            vals.push(v);
        }
    }
    median_f64(&mut vals).unwrap_or(0.0)
}

/// 行の代表 font_size
fn line_rep_font_size(chars: &[TextChar], line: &TextLine) -> Option<f64> {
    let mut vals = Vec::new();
    for &idx in &line.chars {
        let ch = &chars[idx as usize];
        if is_whitespace_str(&ch.text) {
            continue;
        }
        if ch.font_size.is_finite() && ch.font_size > 0.0 {
            vals.push(ch.font_size);
        }
    }
    median_f64(&mut vals)
}

/// 行の進行軸区間
fn line_progress_interval(page: &TextPage, line: &TextLine) -> (f64, f64) {
    let vertical = line.dir == "ttb";
    let mut min_p = f64::INFINITY;
    let mut max_p = f64::NEG_INFINITY;
    let mut any = false;
    for &idx in &line.chars {
        let ch = &page.chars[idx as usize];
        if is_whitespace_str(&ch.text) {
            continue;
        }
        any = true;
        for &(x, y) in &[
            (ch.left, ch.top),
            (ch.right, ch.top),
            (ch.left, ch.bottom),
            (ch.right, ch.bottom),
        ] {
            let (px, py) = derotate_xy(x, y, line.rot);
            let p = if vertical { py } else { px };
            min_p = min_p.min(p);
            max_p = max_p.max(p);
        }
    }
    if any {
        (min_p, max_p)
    } else {
        (0.0, 0.0)
    }
}

/// 進行軸区間の共有長が正か
fn intervals_overlap(a: (f64, f64), b: (f64, f64)) -> bool {
    let lo = a.0.max(b.0);
    let hi = a.1.min(b.1);
    hi > lo
}

/// 有限値の中央値
fn median_f64(values: &mut [f64]) -> Option<f64> {
    let n = values.len();
    if n == 0 {
        return None;
    }
    let cmp = |a: &f64, b: &f64| a.total_cmp(b);
    if n % 2 == 1 {
        let mid = n / 2;
        values.select_nth_unstable_by(mid, cmp);
        Some(values[mid])
    } else {
        let hi = n / 2;
        values.select_nth_unstable_by(hi, cmp);
        let upper = values[hi];
        let lower = values[..hi]
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        Some((lower + upper) / 2.0)
    }
}

/// ラテン文字か
fn is_latin_letter(c: char) -> bool {
    matches!(c,
        'A'..='Z'
        | 'a'..='z'
        | '\u{00C0}'..='\u{00D6}'
        | '\u{00D8}'..='\u{00F6}'
        | '\u{00F8}'..='\u{00FF}'
        | '\u{0100}'..='\u{017F}'
        | '\u{0180}'..='\u{024F}'
        | '\u{1E00}'..='\u{1EFF}'
    )
}

/// CJK 文字種か
fn is_cjk(c: char) -> bool {
    matches!(c,
        // Hangul Jamo
        '\u{1100}'..='\u{11FF}'
        // CJK Radicals Supplement
        | '\u{2E80}'..='\u{2EFF}'
        // Kangxi Radicals
        | '\u{2F00}'..='\u{2FDF}'
        // Ideographic Description Characters
        | '\u{2FF0}'..='\u{2FFF}'
        // CJK Symbols and Punctuation
        | '\u{3000}'..='\u{303F}'
        // Hiragana
        | '\u{3040}'..='\u{309F}'
        // Katakana
        | '\u{30A0}'..='\u{30FF}'
        // Bopomofo
        | '\u{3100}'..='\u{312F}'
        // Hangul Compatibility Jamo
        | '\u{3130}'..='\u{318F}'
        // Kanbun / Bopomofo Extended
        | '\u{3190}'..='\u{31EF}'
        // Katakana Phonetic Extensions
        | '\u{31F0}'..='\u{31FF}'
        // Enclosed CJK Letters and Months
        | '\u{3200}'..='\u{32FF}'
        // CJK Compatibility
        | '\u{3300}'..='\u{33FF}'
        // CJK Unified Ideographs Extension A
        | '\u{3400}'..='\u{4DBF}'
        // CJK Unified Ideographs
        | '\u{4E00}'..='\u{9FFF}'
        // Hangul Syllables
        | '\u{AC00}'..='\u{D7AF}'
        // CJK Compatibility Ideographs
        | '\u{F900}'..='\u{FAFF}'
        // Vertical Forms
        | '\u{FE10}'..='\u{FE1F}'
        // CJK Compatibility Forms
        | '\u{FE30}'..='\u{FE4F}'
        // Halfwidth and Fullwidth Forms
        | '\u{FF00}'..='\u{FFEF}'
        // CJK Unified Ideographs Extension B〜 / Compatibility Supplement
        | '\u{20000}'..='\u{2FA1F}'
        // CJK Unified Ideographs Extension G〜H
        | '\u{30000}'..='\u{323AF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    fn font(vertical: bool) -> TextFont {
        TextFont {
            name: "F".into(),
            ascent: 0.8,
            descent: -0.2,
            vertical,
        }
    }

    fn ch(
        text: &str,
        x: f64,
        y: f64,
        adv_x: f64,
        adv_y: f64,
        font_size: f64,
    ) -> TextChar {
        TextChar {
            text: text.into(),
            left: x,
            right: x + adv_x.abs().max(1.0),
            top: y,
            bottom: y + font_size,
            transform: [font_size, 0.0, 0.0, -font_size, x, y],
            advance: [adv_x, adv_y],
            font: 0,
            font_size,
            rot: 0,
            upright: true,
            synthetic: false,
        }
    }

    fn ch_at(
        text: &str,
        x: f64,
        y: f64,
        adv_x: f64,
        adv_y: f64,
        font_size: f64,
        font_idx: u32,
        vertical: bool,
    ) -> TextChar {
        TextChar {
            text: text.into(),
            left: x,
            right: x + if vertical {
                font_size
            } else {
                adv_x.abs().max(1.0)
            },
            top: y,
            bottom: y + if vertical {
                adv_y.abs().max(font_size)
            } else {
                font_size
            },
            transform: [font_size, 0.0, 0.0, -font_size, x, y],
            advance: [adv_x, adv_y],
            font: font_idx,
            font_size,
            rot: 0,
            upright: !vertical,
            synthetic: false,
        }
    }

    fn ch_rot(text: &str, x: f64, y: f64, advance: f64, font_size: f64, rot: i32) -> TextChar {
        let (transform, glyph_advance, left, right, top, bottom) = match rot {
            90 => (
                [0.0, -font_size, -font_size, 0.0, x, y],
                [0.0, -advance],
                x - font_size,
                x,
                y - advance,
                y,
            ),
            180 => (
                [-font_size, 0.0, 0.0, font_size, x, y],
                [-advance, 0.0],
                x - advance,
                x,
                y - font_size,
                y,
            ),
            270 => (
                [0.0, font_size, font_size, 0.0, x, y],
                [0.0, advance],
                x,
                x + font_size,
                y,
                y + advance,
            ),
            _ => (
                [font_size, 0.0, 0.0, -font_size, x, y],
                [advance, 0.0],
                x,
                x + advance,
                y,
                y + font_size,
            ),
        };
        TextChar {
            text: text.into(),
            left,
            right,
            top,
            bottom,
            transform,
            advance: glyph_advance,
            font: 0,
            font_size,
            rot,
            upright: rot == 0,
            synthetic: false,
        }
    }

    fn page(chars: Vec<TextChar>) -> TextPage {
        TextPage {
            width: 600.0,
            height: 800.0,
            fonts: vec![font(false)],
            chars,
        }
    }

    fn cell(text: &str, left: f64, top: f64, right: f64, bottom: f64) -> Cell {
        Cell {
            text: text.into(),
            bbox: BBox {
                x0: left,
                top,
                x1: right,
                bottom,
            },
        }
    }

    fn table_one(cells: Vec<Vec<Cell>>, left: f64, top: f64, right: f64, bottom: f64) -> Table {
        Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: left,
                top,
                x1: right,
                bottom,
            },
            n_rows: cells.len(),
            n_cols: cells.first().map(|r| r.len()).unwrap_or(0),
            data: cells,
        }
    }

    fn text_blocks(blocks: &[DocBlock]) -> Vec<&TextBlock> {
        blocks
            .iter()
            .filter_map(|b| match b {
                DocBlock::Text(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    fn word_line(text: &str, x: f64, y: f64, fs: f64) -> Vec<TextChar> {
        let mut chars = Vec::new();
        let mut cx = x;
        let adv = fs * 0.5;
        for c in text.chars() {
            let s = c.to_string();
            chars.push(ch(&s, cx, y, adv, 0.0, fs));
            cx += adv;
        }
        chars
    }

    #[test]
    fn paragraph_merges_close_lines() {
        let fs = 10.0;
        let adv = 5.0;
        // 行送り 15 ≤ 1.8*10 → 併合
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs),
            ch("B", adv, 100.0, adv, 0.0, fs),
            ch("C", 0.0, 115.0, adv, 0.0, fs),
            ch("D", adv, 115.0, adv, 0.0, fs),
        ]);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "AB CD");
        assert_eq!(tb[0].lines.len(), 2);
        assert_eq!(tb[0].kind, TextBlockKind::Paragraph);
    }

    #[test]
    fn paragraph_splits_on_large_leading() {
        let fs = 10.0;
        let adv = 5.0;
        // 行送り 20 > 1.8*10 → 分割
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs),
            ch("B", 0.0, 120.0, adv, 0.0, fs),
        ]);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 2);
        assert_eq!(tb[0].text, "A");
        assert_eq!(tb[1].text, "B");
    }

    #[test]
    fn paragraph_splits_on_font_size_ratio() {
        let adv = 5.0;
        // 10 と 20 で比 2.0 > 1.33
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, 10.0),
            ch("B", 0.0, 112.0, adv, 0.0, 20.0),
        ]);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 2);
    }

    #[test]
    fn paragraph_splits_on_progress_non_overlap() {
        let fs = 10.0;
        let adv = 5.0;
        // 進行軸区間が重ならない
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs),
            ch("B", 100.0, 110.0, adv, 0.0, fs),
        ]);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 2);
    }

    #[test]
    fn join_latin_inserts_space() {
        let fs = 10.0;
        let mut chars = word_line("hello", 0.0, 100.0, fs);
        chars.extend(word_line("world", 0.0, 115.0, fs));
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "hello world");
    }

    #[test]
    fn join_cjk_no_space() {
        let fs = 10.0;
        let mut chars = word_line("日本", 0.0, 100.0, fs);
        chars.extend(word_line("語文", 0.0, 115.0, fs));
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "日本語文");
    }

    #[test]
    fn join_cjk_ext_b_no_space() {
        let fs = 10.0;
        let mut chars = word_line("株式", 0.0, 100.0, fs);
        chars.extend(word_line("\u{20BB7}野", 0.0, 115.0, fs));
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "株式\u{20BB7}野");
    }

    #[test]
    fn join_hyphen_removed_when_latin_lowercase() {
        let fs = 10.0;
        let mut chars = word_line("hyphen-", 0.0, 100.0, fs);
        chars.extend(word_line("ation", 0.0, 115.0, fs));
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "hyphenation");
    }

    #[test]
    fn join_hyphen_kept_no_space_when_not_lowercase() {
        let fs = 10.0;
        let mut chars = word_line("FOO-", 0.0, 100.0, fs);
        chars.extend(word_line("BAR", 0.0, 115.0, fs));
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "FOO-BAR");
    }

    #[test]
    fn join_soft_hyphen_always_removed() {
        let fs = 10.0;
        let mut chars = word_line("soft\u{00AD}", 0.0, 100.0, fs);
        chars.extend(word_line("ware", 0.0, 115.0, fs));
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "software");
    }

    #[test]
    fn join_hyphen_digit_keeps_hyphen_no_space() {
        let fs = 10.0;
        let mut chars = word_line("end-", 0.0, 100.0, fs);
        chars.extend(word_line("123", 0.0, 115.0, fs));
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "end-123");
    }

    #[test]
    fn table_excludes_fully_owned_line() {
        let fs = 10.0;
        let adv = 5.0;
        // 表セル内の文字と外の文字
        let p = page(vec![
            ch("T", 10.0, 10.0, adv, 0.0, fs),
            ch("A", 15.0, 10.0, adv, 0.0, fs),
            ch("X", 0.0, 100.0, adv, 0.0, fs),
        ]);
        let t = table_one(
            vec![vec![cell("TA", 5.0, 5.0, 40.0, 30.0)]],
            5.0,
            5.0,
            40.0,
            30.0,
        );
        let blocks = build_blocks(&p, &[t]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "X");
        assert!(matches!(blocks[0], DocBlock::Table(_)) || matches!(blocks[1], DocBlock::Table(_)));
    }

    #[test]
    fn table_partial_line_rebuilt() {
        let fs = 10.0;
        let adv = 5.0;
        // 同一行: 表内 A と表外 B（x を離して同一ベースライン）
        let p = page(vec![
            ch("A", 10.0, 100.0, adv, 0.0, fs),
            ch("B", 80.0, 100.0, adv, 0.0, fs),
        ]);
        let t = table_one(
            vec![vec![cell("A", 5.0, 95.0, 30.0, 120.0)]],
            5.0,
            95.0,
            30.0,
            120.0,
        );
        let blocks = build_blocks(&p, &[t]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "B");
        assert_eq!(tb[0].lines[0].chars, vec![1]);
        assert_eq!(tb[0].lines[0].words[0].text, "B");
    }

    #[test]
    fn table_bbox_non_cell_glyph_remains() {
        let fs = 10.0;
        let adv = 5.0;
        // 表 bbox 内だがセル外（キャプション想定）
        let p = page(vec![
            ch("C", 10.0, 50.0, adv, 0.0, fs),
            ch("T", 10.0, 100.0, adv, 0.0, fs),
        ]);
        let t = Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 40.0,
                x1: 100.0,
                bottom: 130.0,
            },
            n_rows: 1,
            n_cols: 1,
            data: vec![vec![cell("T", 5.0, 95.0, 40.0, 120.0)]],
        };
        let blocks = build_blocks(&p, &[t]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "C");
    }

    #[test]
    fn invalid_cells_do_not_own_glyphs() {
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![ch("A", 10.0, 10.0, adv, 0.0, fs)]);
        // 空 text・ゼロ面積は無効
        let t = table_one(
            vec![vec![
                cell("", 5.0, 5.0, 40.0, 30.0),
                Cell {
                    text: "Z".into(),
                    bbox: BBox {
                        x0: 5.0,
                        top: 5.0,
                        x1: 5.0,
                        bottom: 30.0,
                    },
                },
            ]],
            5.0,
            5.0,
            40.0,
            30.0,
        );
        let blocks = build_blocks(&p, &[t]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "A");
    }

    #[test]
    fn reading_order_interleaves_table_and_paragraph() {
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("T", 0.0, 10.0, adv, 0.0, fs), // top para
            ch("B", 0.0, 200.0, adv, 0.0, fs), // bottom para
        ]);
        let t = table_one(
            vec![vec![cell("X", 50.0, 80.0, 90.0, 120.0)]],
            50.0,
            80.0,
            90.0,
            120.0,
        );
        let blocks = build_blocks(&p, &[t]);
        assert_eq!(blocks.len(), 3);
        match &blocks[0] {
            DocBlock::Text(tb) => assert_eq!(tb.text, "T"),
            _ => panic!("expected top text"),
        }
        match &blocks[1] {
            DocBlock::Table(tb) => assert!((tb.bbox.top - 80.0).abs() < 1e-9),
            _ => panic!("expected table"),
        }
        match &blocks[2] {
            DocBlock::Text(tb) => assert_eq!(tb.text, "B"),
            _ => panic!("expected bottom text"),
        }
    }

    #[test]
    fn reading_order_tie_break_by_generation() {
        let fs = 10.0;
        let adv = 5.0;
        // 本文と表が完全に同座標 (top,left)
        let p = page(vec![ch("A", 10.0, 10.0, adv, 0.0, fs)]);
        let t = table_one(
            vec![vec![cell("Z", 100.0, 100.0, 140.0, 130.0)]],
            10.0,
            10.0,
            50.0,
            40.0,
        );
        // 本文 bbox が top=10,left=10、表 bbox も top=10,left=10
        // 表の生成番号 0 < 本文 1 → 表が先
        let blocks = build_blocks(&p, &[t]);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[0], DocBlock::Table(_)));
        assert!(matches!(blocks[1], DocBlock::Text(_)));
    }

    #[test]
    fn empty_page_yields_no_blocks() {
        let p = page(vec![]);
        let blocks = build_blocks(&p, &[]);
        assert!(blocks.is_empty());
    }

    #[test]
    fn tables_only_page() {
        let p = page(vec![]);
        let t = table_one(
            vec![vec![cell("X", 0.0, 0.0, 10.0, 10.0)]],
            0.0,
            0.0,
            10.0,
            10.0,
        );
        let blocks = build_blocks(&p, &[t]);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], DocBlock::Table(_)));
    }

    #[test]
    fn all_lines_table_owned() {
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 10.0, 10.0, adv, 0.0, fs),
            ch("B", 15.0, 10.0, adv, 0.0, fs),
        ]);
        let t = table_one(
            vec![vec![cell("AB", 5.0, 5.0, 40.0, 30.0)]],
            5.0,
            5.0,
            40.0,
            30.0,
        );
        let blocks = build_blocks(&p, &[t]);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], DocBlock::Table(_)));
        assert!(text_blocks(&blocks).is_empty());
    }

    #[test]
    fn rotated_group_builds_paragraph() {
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch_rot("A", 100.0, 60.0, adv, fs, 90),
            ch_rot("B", 100.0, 60.0 - adv, adv, fs, 90),
            ch_rot("C", 100.0, 60.0 - adv * 2.0, adv, fs, 90),
        ]);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].rot, 90);
        assert_eq!(tb[0].dir, "ltr");
        assert_eq!(tb[0].text, "ABC");
    }

    #[test]
    fn mixed_vertical_and_horizontal() {
        let fs = 10.0;
        let adv = 5.0;
        let p = TextPage {
            width: 600.0,
            height: 800.0,
            fonts: vec![font(false), font(true)],
            chars: vec![
                {
                    let mut c = ch("H", 0.0, 100.0, adv, 0.0, fs);
                    c.font = 0;
                    c
                },
                {
                    let mut c = ch("i", adv, 100.0, adv, 0.0, fs);
                    c.font = 0;
                    c
                },
                ch_at("あ", 200.0, 50.0, 0.0, 10.0, fs, 1, true),
                ch_at("い", 200.0, 60.0, 0.0, 10.0, fs, 1, true),
            ],
        };
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 2);
        // 表示座標 top で読む: 縦書き top=50 が先
        assert_eq!(tb[0].dir, "ttb");
        assert_eq!(tb[0].text, "あい");
        assert_eq!(tb[1].dir, "ltr");
        assert_eq!(tb[1].text, "Hi");
    }

    #[test]
    fn json_kind_and_type_fields() {
        let fs = 10.0;
        let p = page(vec![ch("A", 0.0, 100.0, 5.0, 0.0, fs)]);
        let blocks = build_blocks(&p, &[]);
        let json = serde_json::to_string(&blocks[0]).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""kind":"paragraph""#));
        assert!(!json.contains(r#""level""#));
    }

    #[test]
    fn json_heading_shape_for_type() {
        let tb = TextBlock {
            kind: TextBlockKind::Heading { level: 2 },
            role: TextBlockRole::Body,
            text: "Title".into(),
            left: 0.0,
            right: 10.0,
            top: 0.0,
            bottom: 10.0,
            dir: "ltr".into(),
            rot: 0,
            lines: vec![],
        };
        let json = serde_json::to_string(&DocBlock::Text(tb)).unwrap();
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""kind":"heading""#));
        assert!(json.contains(r#""level":2"#));
    }

    /// 本文と候補見出しから DocDoc を組み立てる
    fn doc_from_body_and_titles(
        body_fs: f64,
        body_text: &str,
        titles: &[(f64, &str)],
    ) -> DocDoc {
        let mut chars = Vec::new();
        // 本文を複数グリフで最頻値を確保
        let mut y = 200.0;
        for _ in 0..5 {
            chars.extend(word_line(body_text, 0.0, y, body_fs));
            y += body_fs * 2.5; // 行送りを大きくして段落分割
        }
        y = 20.0;
        for &(fs, text) in titles {
            chars.extend(word_line(text, 0.0, y, fs));
            y += fs * 3.0;
        }
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        DocDoc {
            pages: vec![DocPage {
                page_number: 1,
                width: p.width,
                height: p.height,
                fonts: p.fonts,
                chars: p.chars,
                blocks,
            }],
        }
    }

    fn text_kinds(doc: &DocDoc) -> Vec<(&str, TextBlockKind)> {
        doc.pages
            .iter()
            .flat_map(|p| p.blocks.iter())
            .filter_map(|b| match b {
                DocBlock::Text(t) => Some((t.text.as_str(), t.kind.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn heading_level1_at_1_8x() {
        // 本文 10、見出し 18 → level 1
        let mut doc = doc_from_body_and_titles(10.0, "body", &[(18.0, "H1")]);
        assign_headings(&mut doc);
        let kinds = text_kinds(&doc);
        let h1 = kinds.iter().find(|(t, _)| *t == "H1").unwrap();
        assert_eq!(h1.1, TextBlockKind::Heading { level: 1 });
    }

    #[test]
    fn heading_level2_at_1_4x() {
        // 本文 10、見出し 14 → level 2（1.4 以上 1.8 未満）
        let mut doc = doc_from_body_and_titles(10.0, "body", &[(14.0, "H2")]);
        assign_headings(&mut doc);
        let kinds = text_kinds(&doc);
        let h2 = kinds.iter().find(|(t, _)| *t == "H2").unwrap();
        assert_eq!(h2.1, TextBlockKind::Heading { level: 2 });
    }

    #[test]
    fn heading_level3_at_1_2x() {
        // 本文 10、見出し 12 → level 3（1.2 以上 1.4 未満）
        let mut doc = doc_from_body_and_titles(10.0, "body", &[(12.0, "H3")]);
        assign_headings(&mut doc);
        let kinds = text_kinds(&doc);
        let h3 = kinds.iter().find(|(t, _)| *t == "H3").unwrap();
        assert_eq!(h3.1, TextBlockKind::Heading { level: 3 });
    }

    #[test]
    fn heading_below_1_2x_stays_paragraph() {
        // 本文と同サイズは Paragraph のまま
        let mut doc = doc_from_body_and_titles(10.0, "body", &[(10.0, "same")]);
        assign_headings(&mut doc);
        let kinds = text_kinds(&doc);
        let same = kinds.iter().find(|(t, _)| *t == "same").unwrap();
        assert_eq!(same.1, TextBlockKind::Paragraph);
        for (t, k) in &kinds {
            if *t == "body" {
                assert_eq!(*k, TextBlockKind::Paragraph);
            }
        }
    }

    #[test]
    fn heading_skips_blocks_with_3_plus_lines() {
        // 3 行以上の大サイズブロックは対象外
        let fs_body = 10.0;
        let fs_big = 20.0;
        let mut chars = Vec::new();
        let mut y = 200.0;
        for _ in 0..5 {
            chars.extend(word_line("body", 0.0, y, fs_body));
            y += fs_body * 2.5;
        }
        // 大サイズ 3 行（行送り 15 ≤ 1.8*20 で 1 ブロックに併合）
        chars.extend(word_line("AAA", 0.0, 20.0, fs_big));
        chars.extend(word_line("BBB", 0.0, 35.0, fs_big));
        chars.extend(word_line("CCC", 0.0, 50.0, fs_big));
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        let mut doc = DocDoc {
            pages: vec![DocPage {
                page_number: 1,
                width: p.width,
                height: p.height,
                fonts: p.fonts,
                chars: p.chars,
                blocks,
            }],
        };
        // 3 行ブロックがあること
        let multi = text_blocks(&doc.pages[0].blocks)
            .into_iter()
            .find(|t| t.lines.len() >= 3)
            .expect("3-line block");
        assert_eq!(multi.lines.len(), 3);
        assign_headings(&mut doc);
        let multi = text_blocks(&doc.pages[0].blocks)
            .into_iter()
            .find(|t| t.lines.len() >= 3)
            .unwrap();
        assert_eq!(multi.kind, TextBlockKind::Paragraph);
    }

    #[test]
    fn heading_mode_tie_picks_smaller() {
        // 10.0 と 12.0 が同数 → 本文代表は 10.0
        // 12.0 の単独ブロックは 1.2 倍ちょうど → level 3
        let mut chars = Vec::new();
        let mut y = 100.0;
        for _ in 0..3 {
            chars.extend(word_line("aaaa", 0.0, y, 10.0));
            y += 30.0;
        }
        for _ in 0..3 {
            chars.extend(word_line("bbbb", 0.0, y, 12.0));
            y += 30.0;
        }
        // 候補: 18 → 1.8*10 = level 1（代表が 12 だと 1.5 で level 2 になる）
        chars.extend(word_line("TITLE", 0.0, 20.0, 18.0));
        let p = page(chars);
        let blocks = build_blocks(&p, &[]);
        let mut doc = DocDoc {
            pages: vec![DocPage {
                page_number: 1,
                width: p.width,
                height: p.height,
                fonts: p.fonts,
                chars: p.chars,
                blocks,
            }],
        };
        assign_headings(&mut doc);
        let kinds = text_kinds(&doc);
        let title = kinds.iter().find(|(t, _)| *t == "TITLE").unwrap();
        assert_eq!(
            title.1,
            TextBlockKind::Heading { level: 1 },
            "tie should pick smaller body fs (10), so 18 is level 1"
        );
    }

    #[test]
    fn heading_no_body_samples_is_noop() {
        // 本文ブロックゼロ（表のみ）では何もしない
        let t = table_one(
            vec![vec![cell("X", 0.0, 0.0, 10.0, 10.0)]],
            0.0,
            0.0,
            10.0,
            10.0,
        );
        let p = page(vec![]);
        let blocks = build_blocks(&p, &[t.clone()]);
        let mut doc = DocDoc {
            pages: vec![DocPage {
                page_number: 1,
                width: p.width,
                height: p.height,
                fonts: p.fonts,
                chars: p.chars,
                blocks,
            }],
        };
        // 手動で Heading 相当の Text を足さず、表のみ
        assert!(text_blocks(&doc.pages[0].blocks).is_empty());
        assign_headings(&mut doc);
        assert_eq!(doc.pages[0].blocks.len(), 1);
        assert!(matches!(doc.pages[0].blocks[0], DocBlock::Table(_)));
    }

    #[test]
    fn heading_ignores_table_glyphs_for_body_mode() {
        // 表内の大サイズは母集団に入らない
        let fs_body = 10.0;
        let fs_table = 30.0;
        let mut chars = word_line("body", 0.0, 200.0, fs_body);
        chars.extend(word_line("body", 0.0, 230.0, fs_body));
        chars.extend(word_line("body", 0.0, 260.0, fs_body));
        // 表セル内の大文字
        chars.extend(word_line("TAB", 10.0, 100.0, fs_table));
        // 見出し候補 14 → 本文 10 なら level 2
        chars.extend(word_line("Head", 0.0, 20.0, 14.0));
        let p = page(chars);
        let t = table_one(
            vec![vec![cell("TAB", 5.0, 95.0, 80.0, 140.0)]],
            5.0,
            95.0,
            80.0,
            140.0,
        );
        let blocks = build_blocks(&p, &[t]);
        let mut doc = DocDoc {
            pages: vec![DocPage {
                page_number: 1,
                width: p.width,
                height: p.height,
                fonts: p.fonts,
                chars: p.chars,
                blocks,
            }],
        };
        assign_headings(&mut doc);
        let kinds = text_kinds(&doc);
        let head = kinds.iter().find(|(t, _)| *t == "Head").unwrap();
        assert_eq!(
            head.1,
            TextBlockKind::Heading { level: 2 },
            "table large glyphs must not shift body mode"
        );
    }

    fn text_block(kind: TextBlockKind, text: &str) -> DocBlock {
        DocBlock::Text(TextBlock {
            kind,
            role: TextBlockRole::Body,
            text: text.into(),
            left: 0.0,
            right: 10.0,
            top: 0.0,
            bottom: 10.0,
            dir: "ltr".into(),
            rot: 0,
            lines: vec![],
        })
    }

    fn doc_with_blocks(blocks: Vec<DocBlock>) -> DocDoc {
        DocDoc {
            pages: vec![DocPage {
                page_number: 1,
                width: 600.0,
                height: 800.0,
                fonts: vec![],
                chars: vec![],
                blocks,
            }],
        }
    }

    #[test]
    fn markdown_heading_and_paragraph() {
        let doc = doc_with_blocks(vec![
            text_block(TextBlockKind::Heading { level: 1 }, "Title"),
            text_block(TextBlockKind::Heading { level: 2 }, "Section"),
            text_block(TextBlockKind::Heading { level: 3 }, "Sub"),
            text_block(TextBlockKind::Paragraph, "body text"),
        ]);
        assert_eq!(
            document_to_markdown(&doc),
            "# Title\n\n## Section\n\n### Sub\n\nbody text"
        );
    }

    #[test]
    fn markdown_gfm_table_shape() {
        let t = Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 100.0,
                bottom: 50.0,
            },
            n_rows: 3,
            n_cols: 2,
            data: vec![
                vec![cell("H1", 0.0, 0.0, 10.0, 10.0), cell("H2", 10.0, 0.0, 20.0, 10.0)],
                vec![cell("A", 0.0, 10.0, 10.0, 20.0), cell("B", 10.0, 10.0, 20.0, 20.0)],
                vec![cell("C", 0.0, 20.0, 10.0, 30.0), cell("D", 10.0, 20.0, 20.0, 30.0)],
            ],
        };
        let doc = doc_with_blocks(vec![DocBlock::Table(t)]);
        assert_eq!(
            document_to_markdown(&doc),
            "| H1 | H2 |\n| --- | --- |\n| A | B |\n| C | D |"
        );
    }

    #[test]
    fn markdown_table_single_row_header_only() {
        let t = Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 50.0,
                bottom: 20.0,
            },
            n_rows: 1,
            n_cols: 2,
            data: vec![vec![
                cell("Only", 0.0, 0.0, 10.0, 10.0),
                cell("Header", 10.0, 0.0, 20.0, 10.0),
            ]],
        };
        let doc = doc_with_blocks(vec![DocBlock::Table(t)]);
        assert_eq!(
            document_to_markdown(&doc),
            "| Only | Header |\n| --- | --- |"
        );
    }

    #[test]
    fn markdown_table_empty_cells_from_merge() {
        // 結合セル由来の空セル
        let t = Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 50.0,
                bottom: 30.0,
            },
            n_rows: 2,
            n_cols: 2,
            data: vec![
                vec![cell("H", 0.0, 0.0, 10.0, 10.0), cell("", 10.0, 0.0, 20.0, 10.0)],
                vec![cell("", 0.0, 10.0, 10.0, 20.0), cell("V", 10.0, 10.0, 20.0, 20.0)],
            ],
        };
        let doc = doc_with_blocks(vec![DocBlock::Table(t)]);
        assert_eq!(
            document_to_markdown(&doc),
            "| H |  |\n| --- | --- |\n|  | V |"
        );
    }

    #[test]
    fn markdown_table_pad_and_truncate_cells() {
        let t = Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 50.0,
                bottom: 30.0,
            },
            n_rows: 2,
            n_cols: 2,
            data: vec![
                // 不足: 1 セル → 空セル埋め
                vec![cell("H", 0.0, 0.0, 10.0, 10.0)],
                // 超過: 3 セル → n_cols で打ち切り
                vec![
                    cell("A", 0.0, 10.0, 10.0, 20.0),
                    cell("B", 10.0, 10.0, 20.0, 20.0),
                    cell("DROP", 20.0, 10.0, 30.0, 20.0),
                ],
            ],
        };
        let doc = doc_with_blocks(vec![DocBlock::Table(t)]);
        assert_eq!(
            document_to_markdown(&doc),
            "| H |  |\n| --- | --- |\n| A | B |"
        );
    }

    #[test]
    fn markdown_escape_backslash_before_pipe() {
        // `\` を先に `\\` へ、その後 `|` を `\|` へ
        let t = Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 50.0,
                bottom: 20.0,
            },
            n_rows: 1,
            n_cols: 1,
            data: vec![vec![cell(r"a\b|c", 0.0, 0.0, 10.0, 10.0)]],
        };
        let doc = doc_with_blocks(vec![DocBlock::Table(t)]);
        assert_eq!(
            document_to_markdown(&doc),
            "| a\\\\b\\|c |\n| --- |"
        );
    }

    #[test]
    fn markdown_control_chars_to_space() {
        let mixed = "a\nb\rc\u{2028}d\u{2029}e\tf\u{0001}g";
        let doc = doc_with_blocks(vec![
            text_block(TextBlockKind::Heading { level: 1 }, mixed),
            text_block(TextBlockKind::Paragraph, mixed),
            DocBlock::Table(Table {
                extraction_method: "lattice",
                bbox: BBox {
                    x0: 0.0,
                    top: 0.0,
                    x1: 20.0,
                    bottom: 20.0,
                },
                n_rows: 1,
                n_cols: 1,
                data: vec![vec![cell(mixed, 0.0, 0.0, 10.0, 10.0)]],
            }),
        ]);
        let md = document_to_markdown(&doc);
        assert_eq!(
            md,
            "# a b c d e f g\n\na b c d e f g\n\n| a b c d e f g |\n| --- |"
        );
        assert!(!md.contains('\t'));
        assert!(!md.contains('\u{2028}'));
        assert!(!md.contains('\u{2029}'));
        assert!(!md.contains('\u{0001}'));
    }

    #[test]
    fn markdown_join_pages_and_blocks() {
        let doc = DocDoc {
            pages: vec![
                DocPage {
                    page_number: 1,
                    width: 600.0,
                    height: 800.0,
                    fonts: vec![],
                    chars: vec![],
                    blocks: vec![
                        text_block(TextBlockKind::Paragraph, "p1"),
                        text_block(TextBlockKind::Paragraph, "p2"),
                    ],
                },
                DocPage {
                    page_number: 2,
                    width: 600.0,
                    height: 800.0,
                    fonts: vec![],
                    chars: vec![],
                    blocks: vec![], // 空ページは要素を出さない
                },
                DocPage {
                    page_number: 3,
                    width: 600.0,
                    height: 800.0,
                    fonts: vec![],
                    chars: vec![],
                    blocks: vec![text_block(TextBlockKind::Paragraph, "p3")],
                },
            ],
        };
        let md = document_to_markdown(&doc);
        assert_eq!(md, "p1\n\np2\n\np3");
        assert!(!md.ends_with('\n'));
    }

    #[test]
    fn markdown_empty_doc_and_empty_paragraph() {
        let empty = DocDoc { pages: vec![] };
        assert_eq!(document_to_markdown(&empty), "");

        let empty_pages = DocDoc {
            pages: vec![DocPage {
                page_number: 1,
                width: 1.0,
                height: 1.0,
                fonts: vec![],
                chars: vec![],
                blocks: vec![],
            }],
        };
        assert_eq!(document_to_markdown(&empty_pages), "");

        // 空段落は非空ブロックではない
        let empty_para = doc_with_blocks(vec![
            text_block(TextBlockKind::Paragraph, ""),
            text_block(TextBlockKind::Paragraph, "x"),
        ]);
        assert_eq!(document_to_markdown(&empty_para), "x");
    }

    #[test]
    fn markdown_skips_zero_dim_tables() {
        let zero_rows = Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 10.0,
                bottom: 10.0,
            },
            n_rows: 0,
            n_cols: 2,
            data: vec![],
        };
        let zero_cols = Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 10.0,
                bottom: 10.0,
            },
            n_rows: 2,
            n_cols: 0,
            data: vec![vec![], vec![]],
        };
        let doc = doc_with_blocks(vec![
            DocBlock::Table(zero_rows),
            text_block(TextBlockKind::Paragraph, "keep"),
            DocBlock::Table(zero_cols),
        ]);
        assert_eq!(document_to_markdown(&doc), "keep");
    }

    /// 制御文字・空白のみの見出し・段落は Markdown に出さない
    #[test]
    fn markdown_skips_whitespace_only_text_blocks() {
        let doc = doc_with_blocks(vec![
            text_block(TextBlockKind::Heading { level: 1 }, "\t\n\u{0001}"),
            text_block(TextBlockKind::Paragraph, "\r\n  \t"),
            text_block(TextBlockKind::Paragraph, "keep"),
            text_block(TextBlockKind::Heading { level: 2 }, "   "),
            text_block(TextBlockKind::Paragraph, "\u{2028}\u{2029}"),
        ]);
        assert_eq!(document_to_markdown(&doc), "keep");
    }

    /// ベースラインの揃った2列ページ（行が列をまたぐ）
    fn two_col_aligned_page() -> TextPage {
        let fs = 10.0;
        let adv = 5.0;
        let mut chars = Vec::new();
        let left = ["A", "B", "C", "D", "E"];
        let right = ["F", "G", "H", "I", "J"];
        for i in 0..5 {
            let y = 20.0 + i as f64 * 15.0;
            chars.push(ch(left[i], 20.0, y, adv, 0.0, fs));
            chars.push(ch(right[i], 100.0, y, adv, 0.0, fs));
        }
        TextPage {
            width: 200.0,
            height: 300.0,
            fonts: vec![font(false)],
            chars,
        }
    }

    #[test]
    fn columns_aligned_two_col_reading_order() {
        let p = two_col_aligned_page();
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 2);
        assert_eq!(tb[0].text, "A B C D E");
        assert_eq!(tb[1].text, "F G H I J");
        assert!(tb[0].left < tb[1].left);
    }

    #[test]
    fn columns_line_fragment_whitespace_and_bbox() {
        let fs = 10.0;
        let adv = 5.0;
        let mut chars = Vec::new();
        // ストリーム順と進行軸順が逆転する行:
        // chars 配列は右語 F → 列間空白 → 左語 A → 先頭空白相当の順で積み、
        // 表示 x は A が左・F が右。build_text_lines が進行軸順に並べ替えた
        // line.chars の相対順を断片が保つことを検証する
        // インデックス: 0=F(右), 1=空白(列間), 2=A(左), 3=空白(A 左隣)
        chars.push(ch("F", 100.0, 20.0, adv, 0.0, fs));
        chars.push(ch(" ", 50.0, 20.0, 5.0, 0.0, fs));
        chars.push(ch("A", 20.0, 20.0, adv, 0.0, fs));
        chars.push(ch(" ", 10.0, 20.0, 5.0, 0.0, fs));
        let left = ["B", "C", "D", "E"];
        let right = ["G", "H", "I", "J"];
        for i in 0..4 {
            let y = 20.0 + (i + 1) as f64 * 15.0;
            // 右列を先にストリーム投入（x は左が小さい）
            chars.push(ch(right[i], 100.0, y, adv, 0.0, fs));
            chars.push(ch(left[i], 20.0, y, adv, 0.0, fs));
        }
        let p = TextPage {
            width: 200.0,
            height: 300.0,
            fonts: vec![font(false)],
            chars,
        };
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 2);
        assert_eq!(tb[0].text, "A B C D E");
        assert_eq!(tb[1].text, "F G H I J");

        let left0 = &tb[0].lines[0];
        assert_eq!(left0.words.len(), 1);
        assert_eq!(left0.words[0].text, "A");
        // 先頭空白は直後の単語（A）側、列間空白は直前の単語（A）側
        let left_ws: Vec<_> = left0
            .chars
            .iter()
            .filter(|&&i| is_whitespace_str(&p.chars[i as usize].text))
            .copied()
            .collect();
        assert_eq!(left_ws.len(), 2);
        // 進行軸順: 空白(x=10, idx3) → A(idx2) → 空白(x=50, idx1)
        // 断片 chars は元 line.chars の相対順（進行軸順）を保ち、
        // ストリーム index 昇順ではない
        assert_eq!(left0.chars, vec![3, 2, 1]);
        // bbox は非空白のみ（A）
        assert!((left0.left - 20.0).abs() < 1e-9);
        assert!((left0.right - 25.0).abs() < 1e-9);

        let right0 = &tb[1].lines[0];
        assert_eq!(right0.words.len(), 1);
        assert_eq!(right0.words[0].text, "F");
        assert!(right0
            .chars
            .iter()
            .all(|&i| !is_whitespace_str(&p.chars[i as usize].text)));
        assert_eq!(right0.chars, vec![0]);
        assert!((right0.left - 100.0).abs() < 1e-9);
    }

    #[test]
    fn columns_staggered_two_col_leaf_assign_only() {
        let fs = 10.0;
        let adv = 5.0;
        let mut chars = Vec::new();
        let left = ["A", "B", "C", "D", "E"];
        let right = ["F", "G", "H", "I", "J"];
        for i in 0..5 {
            let yl = 20.0 + i as f64 * 16.0;
            let yr = 28.0 + i as f64 * 16.0;
            chars.push(ch(left[i], 20.0, yl, adv, 0.0, fs));
            chars.push(ch(right[i], 100.0, yr, adv, 0.0, fs));
        }
        let p = TextPage {
            width: 200.0,
            height: 300.0,
            fonts: vec![font(false)],
            chars,
        };
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        // 行は元々別なので断片分割は不要。葉割り当てで左→右
        let texts: Vec<&str> = tb.iter().map(|t| t.text.as_str()).collect();
        let joined = texts.join(" ");
        let left_pos = joined.find('A').expect("A");
        let right_pos = joined.find('F').expect("F");
        assert!(left_pos < right_pos);
        for t in &tb {
            for line in &t.lines {
                // 1行1語（列をまたがない）
                assert_eq!(line.words.len(), 1);
            }
        }
    }

    #[test]
    fn columns_table_and_two_col_order() {
        let fs = 10.0;
        let adv = 5.0;
        let mut chars = Vec::new();
        let left = ["A", "B", "C", "D", "E"];
        let right = ["F", "G", "H", "I", "J"];
        for i in 0..5 {
            let y = 40.0 + i as f64 * 15.0;
            chars.push(ch(left[i], 20.0, y, adv, 0.0, fs));
            chars.push(ch(right[i], 100.0, y, adv, 0.0, fs));
        }
        let p = TextPage {
            width: 200.0,
            height: 300.0,
            fonts: vec![font(false)],
            chars,
        };
        // 全幅の表（Y分割で上帯へ）
        let t = table_one(
            vec![vec![cell("T", 20.0, 5.0, 180.0, 24.0)]],
            20.0,
            5.0,
            180.0,
            24.0,
        );
        let blocks = build_blocks(&p, &[t]);
        assert!(matches!(blocks[0], DocBlock::Table(_)));
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 2);
        assert_eq!(tb[0].text, "A B C D E");
        assert_eq!(tb[1].text, "F G H I J");
        // 生成番号: 表0、左本文1、右本文2 → 葉順でも表が先頭
        assert_eq!(blocks.len(), 3);
    }

    #[test]
    fn columns_single_column_matches_legacy() {
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs),
            ch("B", adv, 100.0, adv, 0.0, fs),
            ch("C", 0.0, 115.0, adv, 0.0, fs),
            ch("D", adv, 115.0, adv, 0.0, fs),
        ]);
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        assert_eq!(tb.len(), 1);
        assert_eq!(tb[0].text, "AB CD");
        assert_eq!(tb[0].lines.len(), 2);
        assert_eq!(tb[0].kind, TextBlockKind::Paragraph);
        assert!((tb[0].left - 0.0).abs() < 1e-9);
        assert!((tb[0].top - 100.0).abs() < 1e-9);
    }

    #[test]
    fn columns_vertical_line_stays_one_block() {
        let fs = 10.0;
        let adv = 5.0;
        let mut chars = Vec::new();
        let left = ["A", "B", "C", "D", "E"];
        let right = ["F", "G", "H", "I", "J"];
        for i in 0..5 {
            let y = 20.0 + i as f64 * 15.0;
            chars.push(ch(left[i], 20.0, y, adv, 0.0, fs));
            chars.push(ch(right[i], 100.0, y, adv, 0.0, fs));
        }
        // 縦書き行（OtherLine）
        chars.push(ch_at("あ", 170.0, 30.0, 0.0, 10.0, fs, 1, true));
        chars.push(ch_at("い", 170.0, 40.0, 0.0, 10.0, fs, 1, true));
        let p = TextPage {
            width: 200.0,
            height: 300.0,
            fonts: vec![font(false), font(true)],
            chars,
        };
        let blocks = build_blocks(&p, &[]);
        let tb = text_blocks(&blocks);
        let vert: Vec<_> = tb.iter().filter(|t| t.dir == "ttb").collect();
        assert_eq!(vert.len(), 1);
        assert_eq!(vert[0].text, "あい");
        assert_eq!(vert[0].lines.len(), 1);
        assert_eq!(vert[0].lines[0].words.len(), 1);
    }

    #[test]
    fn columns_headings_and_markdown_pipeline() {
        let fs = 10.0;
        let adv = 5.0;
        let mut chars = Vec::new();
        // 左列先頭を大きめ見出し相当
        chars.push(ch("T", 20.0, 20.0, adv, 0.0, 18.0));
        chars.push(ch("U", 100.0, 20.0, adv, 0.0, fs));
        let left = ["B", "C", "D", "E"];
        let right = ["V", "W", "X", "Y"];
        for i in 0..4 {
            let y = 20.0 + (i + 1) as f64 * 15.0;
            chars.push(ch(left[i], 20.0, y, adv, 0.0, fs));
            chars.push(ch(right[i], 100.0, y, adv, 0.0, fs));
        }
        let p = TextPage {
            width: 200.0,
            height: 300.0,
            fonts: vec![font(false)],
            chars,
        };
        let blocks = build_blocks(&p, &[]);
        let mut doc = DocDoc {
            pages: vec![DocPage {
                page_number: 1,
                width: p.width,
                height: p.height,
                fonts: p.fonts.clone(),
                chars: p.chars.clone(),
                blocks,
            }],
        };
        assign_headings(&mut doc);
        let md = document_to_markdown(&doc);
        // 左列が先に出る
        let pos_t = md.find('T').expect("T");
        let pos_u = md.find('U').expect("U");
        assert!(pos_t < pos_u);
        let kinds = text_kinds(&doc);
        let title = kinds.iter().find(|(t, _)| *t == "T").unwrap();
        assert_eq!(title.1, TextBlockKind::Heading { level: 1 });
        assert!(md.starts_with("# T") || md.contains("# T"));
    }

    // --- specs/13 ヘッダ・フッタ ---

    /// 1行・代表 font_size 付きの本文ブロックを作る
    fn furniture_block(
        text: &str,
        left: f64,
        top: f64,
        right: f64,
        bottom: f64,
        fs: f64,
        chars: &mut Vec<TextChar>,
    ) -> TextBlock {
        let mut indices = Vec::new();
        let mut x = left;
        for ch_c in text.chars() {
            let s = ch_c.to_string();
            let idx = chars.len() as u32;
            chars.push(TextChar {
                text: s.clone(),
                left: x,
                right: x + 1.0,
                top,
                bottom,
                transform: [fs, 0.0, 0.0, -fs, x, top],
                advance: [1.0, 0.0],
                font: 0,
                font_size: fs,
                rot: 0,
                upright: true,
                synthetic: false,
            });
            indices.push(idx);
            x += 1.0;
        }
        let word = TextWord {
            text: text.into(),
            left,
            right,
            top,
            bottom,
            chars: indices.clone(),
        };
        let line = TextLine {
            left,
            right,
            top,
            bottom,
            dir: "ltr".into(),
            rot: 0,
            words: vec![word],
            chars: indices,
        };
        TextBlock {
            kind: TextBlockKind::Paragraph,
            role: TextBlockRole::Body,
            text: text.into(),
            left,
            right,
            top,
            bottom,
            dir: "ltr".into(),
            rot: 0,
            lines: vec![line],
        }
    }

    fn furniture_page(
        page_number: usize,
        width: f64,
        height: f64,
        blocks: Vec<TextBlock>,
        chars: Vec<TextChar>,
    ) -> DocPage {
        DocPage {
            page_number,
            width,
            height,
            fonts: vec![],
            chars,
            blocks: blocks.into_iter().map(DocBlock::Text).collect(),
        }
    }

    fn roles_of(doc: &DocDoc) -> Vec<Vec<TextBlockRole>> {
        doc.pages
            .iter()
            .map(|p| {
                p.blocks
                    .iter()
                    .filter_map(|b| match b {
                        DocBlock::Text(t) => Some(t.role),
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn extract_options_detect_header_footer_default_false() {
        assert!(!ExtractOptions::default().detect_header_footer);
    }

    #[test]
    fn furniture_band_boundary_exactly_one_sixth() {
        // height=600 → 1/6=100, 5/6=500。bottom ちょうど 100 は header 候補
        let h = 600.0;
        let mut ca = Vec::new();
        let mut cb = Vec::new();
        let h1 = furniture_block("HDR", 10.0, 80.0, 50.0, 100.0, 10.0, &mut ca);
        let bd1 = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut ca);
        let h2 = furniture_block("HDR", 10.0, 80.0, 50.0, 100.0, 10.0, &mut cb);
        let bd2 = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut cb);
        let mut doc = DocDoc {
            pages: vec![
                furniture_page(1, 400.0, h, vec![h1, bd1], ca),
                furniture_page(2, 400.0, h, vec![h2, bd2], cb),
            ],
        };
        assign_header_footer(&mut doc);
        assert_eq!(
            roles_of(&doc),
            vec![
                vec![TextBlockRole::Header, TextBlockRole::Body],
                vec![TextBlockRole::Header, TextBlockRole::Body],
            ]
        );

        // footer: top ちょうど 500
        let mut ca = Vec::new();
        let mut cb = Vec::new();
        let f1 = furniture_block("FTR", 10.0, 500.0, 50.0, 520.0, 10.0, &mut ca);
        let bd1 = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut ca);
        let f2 = furniture_block("FTR", 10.0, 500.0, 50.0, 520.0, 10.0, &mut cb);
        let bd2 = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut cb);
        let mut doc = DocDoc {
            pages: vec![
                furniture_page(1, 400.0, h, vec![bd1, f1], ca),
                furniture_page(2, 400.0, h, vec![bd2, f2], cb),
            ],
        };
        assign_header_footer(&mut doc);
        let roles = roles_of(&doc);
        assert!(roles[0].contains(&TextBlockRole::Footer));
        assert!(roles[1].contains(&TextBlockRole::Footer));
    }

    #[test]
    fn furniture_edge_gap_exactly_30() {
        let h = 600.0;
        // 2要素ヘッダ: 1つ目 bottom=20, 2つ目 top=50 → gap=30
        let mut pages = Vec::new();
        for pi in 0..2 {
            let mut chars = Vec::new();
            let a = furniture_block("A", 10.0, 5.0, 40.0, 20.0, 10.0, &mut chars);
            let b = furniture_block("B", 10.0, 50.0, 40.0, 65.0, 10.0, &mut chars);
            let body = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut chars);
            pages.push(furniture_page(pi + 1, 400.0, h, vec![a, b, body], chars));
        }
        let mut doc = DocDoc { pages };
        assign_header_footer(&mut doc);
        for page in &doc.pages {
            let roles: Vec<_> = page
                .blocks
                .iter()
                .filter_map(|b| match b {
                    DocBlock::Text(t) => Some((t.text.as_str(), t.role)),
                    _ => None,
                })
                .collect();
            assert_eq!(roles[0], ("A", TextBlockRole::Header));
            assert_eq!(roles[1], ("B", TextBlockRole::Header));
            assert_eq!(roles[2], ("body", TextBlockRole::Body));
        }

        // gap 30 超（31）で2要素目は不成立
        let mut pages = Vec::new();
        for pi in 0..2 {
            let mut chars = Vec::new();
            let a = furniture_block("A", 10.0, 5.0, 40.0, 20.0, 10.0, &mut chars);
            let b = furniture_block("B", 10.0, 51.0, 40.0, 66.0, 10.0, &mut chars);
            let body = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut chars);
            pages.push(furniture_page(pi + 1, 400.0, h, vec![a, b, body], chars));
        }
        let mut doc = DocDoc { pages };
        assign_header_footer(&mut doc);
        for page in &doc.pages {
            let roles: Vec<_> = page
                .blocks
                .iter()
                .filter_map(|b| match b {
                    DocBlock::Text(t) => Some((t.text.as_str(), t.role)),
                    _ => None,
                })
                .collect();
            assert_eq!(roles[0], ("A", TextBlockRole::Header));
            assert_eq!(roles[1], ("B", TextBlockRole::Body));
        }
    }

    #[test]
    fn furniture_font_ratio_exactly_1_05() {
        let h = 600.0;
        let mut ca = Vec::new();
        let mut cb = Vec::new();
        let a = furniture_block("HDR", 10.0, 10.0, 50.0, 25.0, 10.0, &mut ca);
        let ba = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut ca);
        let b = furniture_block("HDR", 10.0, 10.0, 50.0, 25.0, 10.5, &mut cb);
        let bb = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut cb);
        let mut doc = DocDoc {
            pages: vec![
                furniture_page(1, 400.0, h, vec![a, ba], ca),
                furniture_page(2, 400.0, h, vec![b, bb], cb),
            ],
        };
        assign_header_footer(&mut doc);
        assert_eq!(roles_of(&doc)[0][0], TextBlockRole::Header);
        assert_eq!(roles_of(&doc)[1][0], TextBlockRole::Header);

        // 1.05 超
        let mut ca = Vec::new();
        let mut cb = Vec::new();
        let a = furniture_block("HDR", 10.0, 10.0, 50.0, 25.0, 10.0, &mut ca);
        let ba = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut ca);
        let b = furniture_block("HDR", 10.0, 10.0, 50.0, 25.0, 10.51, &mut cb);
        let bb = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut cb);
        let mut doc = DocDoc {
            pages: vec![
                furniture_page(1, 400.0, h, vec![a, ba], ca),
                furniture_page(2, 400.0, h, vec![b, bb], cb),
            ],
        };
        assign_header_footer(&mut doc);
        assert_eq!(roles_of(&doc)[0][0], TextBlockRole::Body);
        assert_eq!(roles_of(&doc)[1][0], TextBlockRole::Body);
    }

    #[test]
    fn furniture_bbox_touch_only_no_match() {
        let h = 600.0;
        let mut ca = Vec::new();
        let mut cb = Vec::new();
        // 接するだけ: a.right == b.left
        let a = furniture_block("HDR", 10.0, 10.0, 50.0, 25.0, 10.0, &mut ca);
        let ba = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut ca);
        let b = furniture_block("HDR", 50.0, 10.0, 90.0, 25.0, 10.0, &mut cb);
        let bb = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut cb);
        let mut doc = DocDoc {
            pages: vec![
                furniture_page(1, 400.0, h, vec![a, ba], ca),
                furniture_page(2, 400.0, h, vec![b, bb], cb),
            ],
        };
        assign_header_footer(&mut doc);
        assert_eq!(roles_of(&doc)[0][0], TextBlockRole::Body);
        assert_eq!(roles_of(&doc)[1][0], TextBlockRole::Body);
    }

    #[test]
    fn furniture_same_top_tiebreak_by_block_index() {
        let h = 600.0;
        // 同一 top の2候補: 添字昇順で C_p が並ぶ。各ページ同じ text 列で一致
        let mut pages = Vec::new();
        for pi in 0..2 {
            let mut chars = Vec::new();
            let a = furniture_block("A", 10.0, 10.0, 40.0, 25.0, 10.0, &mut chars);
            let b = furniture_block("B", 50.0, 10.0, 80.0, 25.0, 10.0, &mut chars);
            let body = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut chars);
            pages.push(furniture_page(pi + 1, 400.0, h, vec![a, b, body], chars));
        }
        let mut doc = DocDoc { pages };
        assign_header_footer(&mut doc);
        // k=0 で A どうし一致、k=1 で B どうし（隣接 gap=10-25 負=重なり相当で OK）
        // top same: A bottom 25, B top 10 → gap = 10-25 = -15 ≤ 30
        for page in &doc.pages {
            let roles: Vec<_> = page
                .blocks
                .iter()
                .filter_map(|b| match b {
                    DocBlock::Text(t) => Some((t.text.as_str(), t.role)),
                    _ => None,
                })
                .collect();
            assert_eq!(roles[0].1, TextBlockRole::Header);
            assert_eq!(roles[1].1, TextBlockRole::Header);
        }
    }

    #[test]
    fn furniture_empty_doc_and_empty_pages() {
        let mut empty = DocDoc { pages: vec![] };
        assign_header_footer(&mut empty);
        assert!(empty.pages.is_empty());

        let mut pages = DocDoc {
            pages: vec![
                furniture_page(1, 400.0, 600.0, vec![], vec![]),
                furniture_page(2, 400.0, 600.0, vec![], vec![]),
            ],
        };
        assign_header_footer(&mut pages);
        assert!(roles_of(&pages).iter().all(|r| r.is_empty()));
    }

    #[test]
    fn furniture_single_page_no_role() {
        let mut chars = Vec::new();
        let h = furniture_block("HDR", 10.0, 10.0, 50.0, 25.0, 10.0, &mut chars);
        let mut doc = DocDoc {
            pages: vec![furniture_page(1, 400.0, 600.0, vec![h], chars)],
        };
        assign_header_footer(&mut doc);
        assert_eq!(roles_of(&doc)[0][0], TextBlockRole::Body);
    }

    #[test]
    fn furniture_non_positive_page_height() {
        let mut ca = Vec::new();
        let mut cb = Vec::new();
        let a = furniture_block("HDR", 10.0, 10.0, 50.0, 25.0, 10.0, &mut ca);
        let b = furniture_block("HDR", 10.0, 10.0, 50.0, 25.0, 10.0, &mut cb);
        let mut doc = DocDoc {
            pages: vec![
                furniture_page(1, 400.0, 0.0, vec![a], ca),
                furniture_page(2, 400.0, f64::NAN, vec![b], cb),
            ],
        };
        assign_header_footer(&mut doc);
        assert_eq!(roles_of(&doc)[0][0], TextBlockRole::Body);
        assert_eq!(roles_of(&doc)[1][0], TextBlockRole::Body);
    }

    #[test]
    fn furniture_mirror_d2_and_rescue() {
        // 3ページ鏡映フッタ: 全幅で bbox 交差する "N  Author" / "Author  N" は骨格不一致
        // d=2 で 1 と 3 が一致、中央は救済
        // 簡約: 全ページ "Page N" 同位置（d=1 で足りる）ではなく、
        // 鏡映は骨格不一致なので d=1 では落ち、d=2 で "Page 1" vs "Page 3" は d=2 で一致
        let h = 600.0;
        let mut pages = Vec::new();
        for (pi, n) in [(0usize, 1u32), (1, 2), (2, 3)] {
            let mut chars = Vec::new();
            let text = format!("Page {n}");
            // 奇偶で x を少しずらすが交差は残す
            let left = if pi % 2 == 0 { 100.0 } else { 120.0 };
            let f = furniture_block(&text, left, 520.0, left + 80.0, 540.0, 10.0, &mut chars);
            let body = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut chars);
            pages.push(furniture_page(pi + 1, 400.0, h, vec![body, f], chars));
        }
        let mut doc = DocDoc { pages };
        assign_header_footer(&mut doc);
        for page in &doc.pages {
            let footer = page.blocks.iter().find_map(|b| match b {
                DocBlock::Text(t) if t.text.starts_with("Page") => Some(t.role),
                _ => None,
            });
            assert_eq!(footer, Some(TextBlockRole::Footer));
        }

        // 3ページで中央のみ骨格が鏡映（数字位置が違う）: "X 1" / "2 X" / "X 3"
        // residual は "X" で一致、値 1,2,3
        let mut pages = Vec::new();
        let texts = ["X 1", "2 X", "X 3"];
        for (pi, text) in texts.iter().enumerate() {
            let mut chars = Vec::new();
            let f = furniture_block(text, 50.0, 520.0, 150.0, 540.0, 10.0, &mut chars);
            let body = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut chars);
            pages.push(furniture_page(pi + 1, 400.0, h, vec![body, f], chars));
        }
        let mut doc = DocDoc { pages };
        assign_header_footer(&mut doc);
        // d=1: 骨格不一致で不成立。d=2: "X 1" vs "X 3" は骨格一致・値+2 で成立
        // 中央 "2 X" は救済（鏡映一致 + 値連続）
        for page in &doc.pages {
            let role = page.blocks.iter().find_map(|b| match b {
                DocBlock::Text(t) if t.text != "body" => Some(t.role),
                _ => None,
            });
            assert_eq!(role, Some(TextBlockRole::Footer), "page {}", page.page_number);
        }
    }

    #[test]
    fn furniture_idempotent_reapply() {
        let h = 600.0;
        let mut pages = Vec::new();
        for pi in 0..2 {
            let mut chars = Vec::new();
            let hd = furniture_block("HDR", 10.0, 10.0, 50.0, 25.0, 10.0, &mut chars);
            let body = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut chars);
            pages.push(furniture_page(pi + 1, 400.0, h, vec![hd, body], chars));
        }
        let mut doc = DocDoc { pages };
        assign_header_footer(&mut doc);
        let once = roles_of(&doc);
        assign_header_footer(&mut doc);
        assert_eq!(roles_of(&doc), once);
        // 既に Header が付いた状態から再適用しても同じ
        assert_eq!(once[0][0], TextBlockRole::Header);
    }

    #[test]
    fn furniture_assign_headings_excludes_non_body() {
        // 大きい font のヘッダが母集団に入ると mode がずれる構成
        let h = 600.0;
        let mut pages = Vec::new();
        for pi in 0..2 {
            let mut chars = Vec::new();
            // ヘッダを多数の大きいグリフで
            let hd = furniture_block("HHHHHH", 10.0, 10.0, 80.0, 30.0, 20.0, &mut chars);
            // 本文
            let body = furniture_block("bbbbbb", 10.0, 200.0, 80.0, 220.0, 10.0, &mut chars);
            // 見出し候補（本文の 1.8 倍 = 18）
            let title = furniture_block("Title", 10.0, 100.0, 80.0, 120.0, 18.0, &mut chars);
            pages.push(furniture_page(
                pi + 1,
                400.0,
                h,
                vec![hd, title, body],
                chars,
            ));
        }
        let mut doc = DocDoc { pages };
        assign_header_footer(&mut doc);
        assign_headings(&mut doc);
        // ヘッダ除外後の mode は 10 → Title 18 は level 1
        for page in &doc.pages {
            for b in &page.blocks {
                if let DocBlock::Text(t) = b {
                    if t.text == "Title" {
                        assert_eq!(t.kind, TextBlockKind::Heading { level: 1 });
                    }
                    if t.text == "HHHHHH" {
                        assert_eq!(t.role, TextBlockRole::Header);
                        assert_eq!(t.kind, TextBlockKind::Paragraph);
                    }
                }
            }
        }
    }

    #[test]
    fn furniture_markdown_skips_non_body() {
        let doc = doc_with_blocks(vec![
            DocBlock::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                role: TextBlockRole::Header,
                text: "header".into(),
                left: 0.0,
                right: 10.0,
                top: 0.0,
                bottom: 10.0,
                dir: "ltr".into(),
                rot: 0,
                lines: vec![],
            }),
            text_block(TextBlockKind::Paragraph, "body"),
            DocBlock::Text(TextBlock {
                kind: TextBlockKind::Paragraph,
                role: TextBlockRole::Footer,
                text: "footer".into(),
                left: 0.0,
                right: 10.0,
                top: 0.0,
                bottom: 10.0,
                dir: "ltr".into(),
                rot: 0,
                lines: vec![],
            }),
        ]);
        assert_eq!(document_to_markdown(&doc), "body");
    }

    #[test]
    fn furniture_json_body_role_preserves_paragraph_and_heading_shapes() {
        let tb = TextBlock {
            kind: TextBlockKind::Paragraph,
            role: TextBlockRole::Body,
            text: "x".into(),
            left: 0.0,
            right: 1.0,
            top: 2.0,
            bottom: 3.0,
            dir: "ltr".into(),
            rot: 0,
            lines: vec![],
        };
        let json = serde_json::to_string(&DocBlock::Text(tb)).unwrap();
        assert_eq!(
            json,
            r#"{"type":"text","kind":"paragraph","text":"x","left":0.0,"right":1.0,"top":2.0,"bottom":3.0,"dir":"ltr","rot":0,"lines":[]}"#
        );

        let tb = TextBlock {
            kind: TextBlockKind::Heading { level: 2 },
            role: TextBlockRole::Body,
            text: "Title".into(),
            left: 10.0,
            right: 20.0,
            top: 30.0,
            bottom: 40.0,
            dir: "ttb".into(),
            rot: 90,
            lines: vec![],
        };
        let json = serde_json::to_string(&DocBlock::Text(tb)).unwrap();
        assert_eq!(
            json,
            r#"{"type":"text","kind":"heading","level":2,"text":"Title","left":10.0,"right":20.0,"top":30.0,"bottom":40.0,"dir":"ttb","rot":90,"lines":[]}"#
        );

        let tb = TextBlock {
            kind: TextBlockKind::Paragraph,
            role: TextBlockRole::Header,
            text: "x".into(),
            left: 0.0,
            right: 1.0,
            top: 0.0,
            bottom: 1.0,
            dir: "ltr".into(),
            rot: 0,
            lines: vec![],
        };
        let json = serde_json::to_string(&DocBlock::Text(tb)).unwrap();
        assert!(json.contains(r#""role":"header""#));
    }

    #[test]
    fn furniture_static_header_two_pages() {
        let h = 600.0;
        let mut pages = Vec::new();
        for pi in 0..2 {
            let mut chars = Vec::new();
            let hd = furniture_block("Company", 10.0, 10.0, 80.0, 25.0, 10.0, &mut chars);
            let body = furniture_block("body", 10.0, 200.0, 50.0, 220.0, 10.0, &mut chars);
            pages.push(furniture_page(pi + 1, 400.0, h, vec![hd, body], chars));
        }
        let mut doc = DocDoc { pages };
        assign_header_footer(&mut doc);
        assert_eq!(roles_of(&doc)[0][0], TextBlockRole::Header);
        assert_eq!(roles_of(&doc)[1][0], TextBlockRole::Header);
        assert_eq!(document_to_markdown(&doc), "body\n\nbody");
    }

}
