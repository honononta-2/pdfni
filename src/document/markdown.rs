//! 文書モデルの Markdown 直列化

use super::{
    DocBlock, DocDoc, DocPage, ListMarkerKind, TextBlock, TextBlockKind, TextBlockRole,
    extract_document_from_bytes_with_query, join_line_words, join_paragraph_text,
    list_item_body_lines, list_marker_kind, sanitize_md_text,
};
use crate::error::Result;
use crate::extract::ExtractOptions;
use crate::model::{Cell, Table};
use crate::query::ExtractQuery;
use crate::text::{TextChar, TextFont, TextLine, TextWord};

pub(super) fn document_to_markdown(doc: &DocDoc, escape_markdown: bool) -> String {
    let mut parts = Vec::new();
    for page in &doc.pages {
        for block in &page.blocks {
            if let Some(s) = block_to_markdown(block, page, escape_markdown) {
                parts.push(s);
            }
        }
    }
    parts.join("\n\n")
}

/// 空表・空白のみ本文は None
fn block_to_markdown(block: &DocBlock, page: &DocPage, escape_markdown: bool) -> Option<String> {
    match block {
        DocBlock::Text(tb) => {
            if tb.role != TextBlockRole::Body {
                return None;
            }
            match &tb.kind {
                TextBlockKind::Heading { level } => {
                    let prefix = "#".repeat(*level as usize);
                    let out: Vec<String> = format_text_block_lines(tb, page, escape_markdown)
                        .into_iter()
                        .filter(|l| !l.trim().is_empty())
                        .map(|l| format!("{prefix} {l}"))
                        .collect();
                    if out.is_empty() {
                        return None;
                    }
                    Some(out.join("\n"))
                }
                TextBlockKind::Paragraph => {
                    let body = format_text_block_lines(tb, page, escape_markdown)
                        .into_iter()
                        .filter(|l| !l.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join(HARD_BREAK);
                    if body.trim().is_empty() {
                        return None;
                    }
                    Some(body)
                }
                TextBlockKind::List { ordered } => {
                    list_to_markdown(tb, page, *ordered, escape_markdown)
                }
            }
        }
        DocBlock::Table(table) => table_to_markdown(table),
    }
}

/// 段落内の行を縦に保つハード改行
const HARD_BREAK: &str = "  \n";

/// 行ごとの Markdown 化。書体があればインライン強調
/// 行情報が使えない場合は結合済み text の1要素
fn format_text_block_lines(tb: &TextBlock, page: &DocPage, escape_markdown: bool) -> Vec<String> {
    if join_paragraph_text(&tb.lines) == tb.text {
        if let Some(lines) = styled_md_lines(&tb.lines, &page.fonts, &page.chars, escape_markdown)
        {
            return lines;
        }
        let plain = plain_md_lines(&tb.lines, escape_markdown);
        if !plain.is_empty() {
            return plain;
        }
    }
    vec![format_md_body(&tb.text, escape_markdown)]
}

/// 見出し・段落・リスト項目本文のエスケープ適用
fn format_md_body(text: &str, escape_markdown: bool) -> String {
    if escape_markdown {
        escape_text_block_text(text)
    } else {
        sanitize_md_text(text)
    }
}

fn list_to_markdown(
    tb: &TextBlock,
    page: &DocPage,
    ordered: bool,
    escape_markdown: bool,
) -> Option<String> {
    let mut out_lines = Vec::new();
    for (num, body_lines) in list_item_body_groups(&tb.lines) {
        let rendered = styled_md_lines(&body_lines, &page.fonts, &page.chars, escape_markdown)
            .unwrap_or_else(|| plain_md_lines(&body_lines, escape_markdown));
        let body = rendered.join(HARD_BREAK);
        if body.trim().is_empty() {
            continue;
        }
        if ordered {
            let Some(n) = num else {
                continue;
            };
            out_lines.push(format!("{n}. {body}"));
        } else {
            out_lines.push(format!("- {body}"));
        }
    }
    if out_lines.is_empty() {
        None
    } else {
        Some(out_lines.join("\n"))
    }
}

/// 書体属性
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MdStyle {
    bold: bool,
    italic: bool,
}

impl MdStyle {
    fn normal() -> Self {
        Self {
            bold: false,
            italic: false,
        }
    }

    fn is_normal(self) -> bool {
        !self.bold && !self.italic
    }
}

/// 行内の本文断片
enum MdUnit {
    /// グリフ由来の文字列
    Text { text: String, style: MdStyle },
    /// 単語間に挿入する空白
    Gap(String),
}

/// 行ごとの書体付き Markdown。強調は行内で完結
fn styled_md_lines(
    lines: &[TextLine],
    fonts: &[TextFont],
    chars: &[TextChar],
    escape_markdown: bool,
) -> Option<Vec<String>> {
    if !lines_have_char_refs(lines) {
        return None;
    }
    let mut out = Vec::new();
    for line in lines {
        let units = line_to_md_units(line, fonts, chars)?;
        let runs = coalesce_md_units(&units);
        let rendered = render_md_runs(&runs, escape_markdown);
        if !rendered.trim().is_empty() {
            out.push(rendered);
        }
    }
    Some(out)
}

fn plain_md_lines(lines: &[TextLine], escape_markdown: bool) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        let text = join_line_words(line);
        if text.trim().is_empty() {
            continue;
        }
        out.push(format_md_body(&text, escape_markdown));
    }
    out
}

/// 装飾に使える文字参照があるか
fn lines_have_char_refs(lines: &[TextLine]) -> bool {
    lines
        .iter()
        .any(|l| l.words.iter().any(|w| !w.chars.is_empty()))
}

fn line_to_md_units(
    line: &TextLine,
    fonts: &[TextFont],
    chars: &[TextChar],
) -> Option<Vec<MdUnit>> {
    let mut units = Vec::new();
    for (i, word) in line.words.iter().enumerate() {
        if i > 0 {
            units.push(MdUnit::Gap(" ".into()));
        }
        units.extend(word_to_md_units(word, fonts, chars)?);
    }
    Some(units)
}

fn word_to_md_units(
    word: &TextWord,
    fonts: &[TextFont],
    chars: &[TextChar],
) -> Option<Vec<MdUnit>> {
    if word.chars.is_empty() {
        if word.text.is_empty() {
            return Some(Vec::new());
        }
        // 文字参照が無く本文だけある単語は復元不能
        return None;
    }
    let mut units = Vec::with_capacity(word.chars.len());
    let mut rebuilt = String::new();
    for &ci in &word.chars {
        let Some(ch) = chars.get(ci as usize) else {
            // 不正な文字インデックス
            return None;
        };
        let style = md_style_of_font(fonts, ch.font);
        rebuilt.push_str(&ch.text);
        units.push(MdUnit::Text {
            text: ch.text.clone(),
            style,
        });
    }
    if rebuilt != word.text {
        return None;
    }
    Some(units)
}

fn md_style_of_font(fonts: &[TextFont], font_idx: u32) -> MdStyle {
    match fonts.get(font_idx as usize) {
        Some(f) => MdStyle {
            bold: f.bold,
            italic: f.italic,
        },
        None => MdStyle::normal(),
    }
}

/// 同一書体の断片の結合。間の挿入空白は区間へ取り込み
fn coalesce_md_units(units: &[MdUnit]) -> Vec<(String, MdStyle)> {
    let mut runs: Vec<(String, MdStyle)> = Vec::new();
    let mut pending_gap = String::new();

    for unit in units {
        match unit {
            MdUnit::Gap(g) => pending_gap.push_str(g),
            MdUnit::Text { text, style } => {
                if text.is_empty() {
                    continue;
                }
                if let Some((last_text, last_style)) = runs.last_mut() {
                    if *last_style == *style {
                        last_text.push_str(&pending_gap);
                        last_text.push_str(text);
                        pending_gap.clear();
                        continue;
                    }
                }
                if !pending_gap.is_empty() {
                    if let Some((last_text, last_style)) = runs.last_mut() {
                        if last_style.is_normal() {
                            last_text.push_str(&pending_gap);
                        } else {
                            runs.push((std::mem::take(&mut pending_gap), MdStyle::normal()));
                        }
                    } else {
                        runs.push((std::mem::take(&mut pending_gap), MdStyle::normal()));
                    }
                    pending_gap.clear();
                }
                if let Some((last_text, last_style)) = runs.last_mut() {
                    if *last_style == *style {
                        last_text.push_str(text);
                        continue;
                    }
                }
                runs.push((text.clone(), *style));
            }
        }
    }
    if !pending_gap.is_empty() {
        if let Some((last_text, last_style)) = runs.last_mut() {
            if last_style.is_normal() {
                last_text.push_str(&pending_gap);
            } else {
                runs.push((pending_gap, MdStyle::normal()));
            }
        } else {
            runs.push((pending_gap, MdStyle::normal()));
        }
    }
    runs
}

fn render_md_runs(runs: &[(String, MdStyle)], escape_markdown: bool) -> String {
    let mut out = String::new();
    for (text, style) in runs {
        out.push_str(&render_md_run(text, *style, escape_markdown));
    }
    out
}

fn render_md_run(text: &str, style: MdStyle, escape_markdown: bool) -> String {
    if style.is_normal() {
        return format_md_body(text, escape_markdown);
    }
    // 区間端の空白は強調の外
    let lead = text.chars().take_while(|&c| c == ' ').count();
    let trail = text
        .chars()
        .rev()
        .take_while(|&c| c == ' ')
        .count()
        .min(text.chars().count().saturating_sub(lead));
    let core: String = text
        .chars()
        .skip(lead)
        .take(text.chars().count() - lead - trail)
        .collect();
    let mut out = String::with_capacity(text.len() + 6);
    for _ in 0..lead {
        out.push(' ');
    }
    if !core.is_empty() {
        let body = format_md_body(&core, escape_markdown);
        let marker = if style.bold && style.italic {
            "***"
        } else if style.bold {
            "**"
        } else {
            "*"
        };
        out.push_str(marker);
        out.push_str(&body);
        out.push_str(marker);
    }
    for _ in 0..trail {
        out.push(' ');
    }
    out
}

/// リスト項目の番号とマーカー除去後の本文行
fn list_item_body_groups(lines: &[TextLine]) -> Vec<(Option<u32>, Vec<TextLine>)> {
    let mut entries = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if list_marker_kind(&lines[i]).is_none() {
            i += 1;
            continue;
        }
        let num = match list_marker_kind(&lines[i]) {
            Some(ListMarkerKind::Ordered { n, .. }) => Some(n),
            _ => None,
        };
        let start = i;
        i += 1;
        while i < lines.len() && list_marker_kind(&lines[i]).is_none() {
            i += 1;
        }
        let body_lines = list_item_body_lines(&lines[start..i]);
        entries.push((num, body_lines));
    }
    entries
}

/// GFM パイプテーブルへの直列化
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

/// セル text の制御文字置換と GFM エスケープ
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

/// 見出し・段落 text の制御文字置換と本文エスケープ
fn escape_text_block_text(text: &str) -> String {
    let sanitized = sanitize_md_text(text);
    let mut body = String::with_capacity(sanitized.len() + 1);
    for c in sanitized.chars() {
        if c == '\\' {
            body.push_str("\\\\");
        } else {
            body.push(c);
        }
    }
    // エスケープは先頭の連続 U+0020 直後のブロック構文文字1文字だけ
    let n_space = body.bytes().take_while(|&b| b == b' ').count();
    if let Some(first) = body[n_space..].chars().next() {
        if matches!(first, '#' | '*' | '-' | '+' | '>' | '|') {
            let mut out = String::with_capacity(body.len() + 1);
            out.push_str(&body[..n_space]);
            out.push('\\');
            out.push_str(&body[n_space..]);
            return out;
        }
    }
    body
}

pub fn extract_markdown_from_bytes(
    data: &[u8],
    password: Option<&str>,
    options: &ExtractOptions,
) -> Result<String> {
    extract_markdown_from_bytes_with_query(data, password, options, &ExtractQuery::default())
}

/// ページ・矩形で絞った Markdown 生成
pub fn extract_markdown_from_bytes_with_query(
    data: &[u8],
    password: Option<&str>,
    options: &ExtractOptions,
    query: &ExtractQuery,
) -> Result<String> {
    let doc = extract_document_from_bytes_with_query(data, password, options, query)?;
    Ok(document_to_markdown(&doc, options.escape_markdown))
}
