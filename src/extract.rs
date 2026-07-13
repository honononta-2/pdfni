//! 抽出のオーケストレーション

use crate::bidi::apply_bidi;
use crate::detect_hybrid::{self, HybridSettings};
use crate::detect_lattice::{self, LatticeSettings};
use crate::detect_text::{self, Seg, TextSettings};
use crate::model::{BBox, Cell, Document, Edge, Glyph, Orientation, Page, Table};
use serde::Deserialize;

/// 外部リーダーから受け取る1ページ分の文字と線分
#[derive(Debug, Clone, Deserialize)]
pub struct PagePart {
    pub width: f64,
    pub height: f64,
    pub glyphs: Vec<GlyphPart>,
    pub edges: Vec<EdgePart>,
    /// リーダーが検出用に正規化したページ内容の回転角(0/90/180/270)
    /// 非0のとき width/height と各座標は正規化後の値で 出力時に元へ戻す
    #[serde(default)]
    pub norm_rotate: i32,
    /// 非罫線グラフィックの外接矩形(省略可)
    #[serde(default)]
    pub graphics: Vec<GraphicPart>,
}

/// 外部リーダーから受け取る1文字
#[derive(Debug, Clone, Deserialize)]
pub struct GlyphPart {
    pub ch: char,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    /// デバイス座標のフォントサイズ。無い場合は bbox 高さから推定する従来動作
    #[serde(default)]
    pub font_size: Option<f64>,
    /// 回転していない横書きグリフか。false は表の材料から除外される
    #[serde(default = "upright_default")]
    pub upright: bool,
    /// 軸平行な回転角(0/90/180/270)。軸外の回転は0のまま upright=false で除外される
    #[serde(default)]
    pub rot: i32,
}

fn upright_default() -> bool {
    true
}

/// 外部リーダーから受け取る軸平行な線分
#[derive(Debug, Clone, Deserialize)]
pub struct EdgePart {
    pub orientation: Orientation,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

/// 外部リーダーから受け取る非罫線グラフィックの外接矩形
///
/// 曲線や斜め線を含むパス1回の描画につき1個。チャート領域の判定材料
#[derive(Debug, Clone, Deserialize)]
pub struct GraphicPart {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

/// 実行する検出方式の選択
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DetectMode {
    /// 3方式すべて
    #[default]
    Auto,
    /// 罫線を使う方式のみ
    Ruled,
    /// 文字整列方式のみ
    Borderless,
}

/// ストリーム復号のチェーン累積出力の既定上限
pub(crate) const DEFAULT_MAX_DECODED_BYTES: usize = 512 * 1024 * 1024;

/// 読み取り層のパラメータ
///
/// parts 入力の経路では使われない
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ReaderSettings {
    /// ストリーム復号のチェーン累積出力の上限バイト数
    pub max_decoded_bytes: usize,
}

impl Default for ReaderSettings {
    fn default() -> Self {
        Self {
            max_decoded_bytes: DEFAULT_MAX_DECODED_BYTES,
        }
    }
}

/// 抽出処理のパラメータ
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ExtractOptions {
    pub mode: DetectMode,
    pub lattice: LatticeSettings,
    pub hybrid: HybridSettings,
    pub text: TextSettings,
    /// 読み取り層の設定
    pub reader: ReaderSettings,
    /// ヘッダ・フッタ検出（既定は無効）
    pub detect_header_footer: bool,
    /// リスト検出（既定は有効）
    #[serde(default = "default_detect_lists")]
    pub detect_lists: bool,
    /// 見出し・段落の Markdown 本文エスケープ（既定は有効）
    #[serde(default = "default_escape_markdown")]
    pub escape_markdown: bool,
    /// RTL 文書の bidi 並べ替え（既定は無効）
    #[serde(default)]
    pub bidi: bool,
}

fn default_detect_lists() -> bool {
    true
}

fn default_escape_markdown() -> bool {
    true
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            mode: DetectMode::default(),
            lattice: LatticeSettings::default(),
            hybrid: HybridSettings::default(),
            text: TextSettings::default(),
            reader: ReaderSettings::default(),
            detect_header_footer: false,
            detect_lists: true,
            escape_markdown: true,
            bidi: false,
        }
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F   // CJK記号・句読点
        | 0x3040..=0x30FF // ひらがな・カタカナ
        | 0x3400..=0x4DBF // 拡張漢字A
        | 0x4E00..=0x9FFF // 漢字
        | 0xFF61..=0xFF9F // 半角カナ
    )
}

/// 行内の水平な空白か
fn is_inline_space(c: char) -> bool {
    c == ' ' || c == '\u{3000}' || c == '\t'
}

/// CJK文字どうしの間の空白を除く
fn strip_cjk_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for i in 0..chars.len() {
        if is_inline_space(chars[i]) {
            let prev = out.chars().last();
            let next = chars[i + 1..]
                .iter()
                .find(|&&x| !is_inline_space(x))
                .copied();
            if let (Some(p), Some(n)) = (prev, next) {
                if is_cjk(p) && is_cjk(n) {
                    continue;
                }
            }
        }
        out.push(chars[i]);
    }
    out
}

/// センターY昇順の文字索引
///
/// セル充当の文字走査をページ全文字からY範囲内へ絞る
struct CharIndex {
    by_cy: Vec<(f64, u32)>,
}

impl CharIndex {
    fn new(chars: &[Glyph]) -> Self {
        let mut by_cy: Vec<(f64, u32)> = chars
            .iter()
            .enumerate()
            .map(|(i, g)| (g.bbox.cy(), i as u32))
            .collect();
        by_cy.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        Self { by_cy }
    }

    /// bb にセンターが入る文字の元indexを昇順で返す
    fn in_box(&self, chars: &[Glyph], bb: &BBox) -> Vec<usize> {
        let lo = self.by_cy.partition_point(|&(cy, _)| cy < bb.top);
        let hi = self.by_cy.partition_point(|&(cy, _)| cy < bb.bottom);
        let mut ids: Vec<usize> = self.by_cy[lo..hi]
            .iter()
            .filter(|&&(_, i)| bb.contains_center(&chars[i as usize].bbox))
            .map(|&(_, i)| i as usize)
            .collect();
        ids.sort_unstable();
        ids
    }
}

/// x ソート済み行で、空白の直前・直後の非空白が接触しているか
fn ws_between_touching_solids_x(line: &[(usize, &Glyph)], ws_pos: usize) -> bool {
    let mut prev_end = None;
    for i in (0..ws_pos).rev() {
        let g = line[i].1;
        if !g.ch.is_whitespace() {
            prev_end = Some(g.bbox.x0.max(g.bbox.x1));
            break;
        }
    }
    let mut next_start = None;
    for i in (ws_pos + 1)..line.len() {
        let g = line[i].1;
        if !g.ch.is_whitespace() {
            next_start = Some(g.bbox.x0.min(g.bbox.x1));
            break;
        }
    }
    match (prev_end, next_start) {
        (Some(l1), Some(r0)) => l1 >= r0 - 0.01,
        _ => false,
    }
}

/// 行グリフを x 順に連結（接触する非空白の間の空白は除く）
fn join_line_glyphs(line: &[(usize, &Glyph)]) -> String {
    let mut s = String::new();
    for (i, (_, g)) in line.iter().enumerate() {
        if g.ch.is_whitespace() && ws_between_touching_solids_x(line, i) {
            continue;
        }
        s.push(g.ch);
    }
    s
}

/// セル内テキストの組み立て
fn cell_text(chars: &[Glyph], index: &CharIndex, cell: &BBox, bidi: bool) -> String {
    let mut hits: Vec<(usize, &Glyph)> = index
        .in_box(chars, cell)
        .into_iter()
        .map(|i| (i, &chars[i]))
        .collect();
    if hits.is_empty() {
        return String::new();
    }

    let mut heights: Vec<f64> = hits.iter().map(|(_, g)| g.bbox.height()).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    let median_h = heights[heights.len() / 2];
    let line_tol = (median_h * 0.6).max(0.5);

    hits.sort_by(|a, b| a.1.bbox.bottom.total_cmp(&b.1.bbox.bottom));
    let mut lines: Vec<Vec<(usize, &Glyph)>> = Vec::new();
    let mut cur: Vec<(usize, &Glyph)> = Vec::new();
    let mut last_bottom = f64::NEG_INFINITY;
    for (i, g) in hits {
        if !cur.is_empty() && g.bbox.bottom - last_bottom > line_tol {
            lines.push(std::mem::take(&mut cur));
        }
        last_bottom = g.bbox.bottom;
        cur.push((i, g));
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    // 縦積み文字の原文順での連結
    let solid = |l: &[(usize, &Glyph)]| l.iter().filter(|(_, g)| !g.ch.is_whitespace()).count();
    if lines.len() >= 2
        && lines.iter().all(|l| solid(l) <= 1)
        && lines.iter().filter(|l| solid(l) == 1).count() >= 2
    {
        let mut idxs: Vec<usize> = lines.iter().flatten().map(|&(i, _)| i).collect();
        idxs.sort_unstable();
        let s: String = idxs.iter().map(|&i| chars[i].ch).collect();
        let s = strip_cjk_spaces(s.trim());
        return if bidi {
            apply_bidi(&s, false).str
        } else {
            s
        };
    }

    let mut out = Vec::with_capacity(lines.len());
    for mut line in lines {
        line.sort_by(|a, b| a.1.bbox.x0.total_cmp(&b.1.bbox.x0));
        let s = join_line_glyphs(&line);
        let s = s.trim();
        if !s.is_empty() {
            let s = if bidi {
                apply_bidi(s, false).str
            } else {
                s.to_string()
            };
            out.push(s);
        }
    }
    strip_cjk_spaces(&out.join("\n"))
}

/// 文字を行内の塊に切り分ける
fn build_segments(chars: &[Glyph], scale: Option<f64>) -> Vec<Seg> {
    let solid: Vec<usize> = (0..chars.len())
        .filter(|&i| !chars[i].ch.is_whitespace())
        .collect();
    if solid.is_empty() {
        return Vec::new();
    }
    let mut heights: Vec<f64> = solid.iter().map(|&i| chars[i].bbox.height()).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    let med_h = scale.unwrap_or(heights[heights.len() / 2]);
    let line_tol = (med_h * 0.6).max(0.5);
    let seg_gap = (med_h).max(1.0);

    let mut order = solid;
    order.sort_by(|&a, &b| chars[a].bbox.bottom.total_cmp(&chars[b].bbox.bottom));

    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut last_b = f64::NEG_INFINITY;
    for &i in &order {
        if !cur.is_empty() && chars[i].bbox.bottom - last_b > line_tol {
            lines.push(std::mem::take(&mut cur));
        }
        last_b = chars[i].bbox.bottom;
        cur.push(i);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    // 語間の空白をセル文字に残すための割り当て
    let ranges: Vec<(f64, f64)> = lines
        .iter()
        .map(|l| {
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for &i in l {
                lo = lo.min(chars[i].bbox.bottom);
                hi = hi.max(chars[i].bbox.bottom);
            }
            (lo, hi)
        })
        .collect();
    for (i, g) in chars.iter().enumerate() {
        if !g.ch.is_whitespace() {
            continue;
        }
        let b = g.bbox.bottom;
        if let Some(k) = ranges
            .iter()
            .position(|&(lo, hi)| b >= lo - 0.5 && b <= hi + 0.5)
        {
            lines[k].push(i);
        }
    }

    let mut segs = Vec::new();
    for mut line in lines {
        line.sort_by(|&a, &b| chars[a].bbox.x0.total_cmp(&chars[b].bbox.x0));
        let mut text = String::new();
        let mut bb: Option<BBox> = None;
        let mut last_x1 = f64::NEG_INFINITY;
        let line_refs: Vec<(usize, &Glyph)> = line.iter().map(|&i| (i, &chars[i])).collect();
        for &i in &line {
            let g = &chars[i];
            if bb.is_some() && g.bbox.x0 - last_x1 > seg_gap {
                push_seg(&mut segs, &mut text, &mut bb);
            }
            if g.ch.is_whitespace() {
                // 接触する非空白の間の空白は文字にしない
                let pos = line_refs.iter().position(|(j, _)| *j == i);
                let intrusive = pos.is_some_and(|p| ws_between_touching_solids_x(&line_refs, p));
                if !intrusive && bb.is_some() {
                    text.push(g.ch);
                }
            } else {
                text.push(g.ch);
                bb = Some(match bb {
                    Some(b) => BBox {
                        x0: b.x0.min(g.bbox.x0),
                        top: b.top.min(g.bbox.top),
                        x1: b.x1.max(g.bbox.x1),
                        bottom: b.bottom.max(g.bbox.bottom),
                    },
                    None => g.bbox,
                });
                last_x1 = g.bbox.x1;
            }
        }
        push_seg(&mut segs, &mut text, &mut bb);
    }
    segs
}

fn push_seg(segs: &mut Vec<Seg>, text: &mut String, bb: &mut Option<BBox>) {
    if let Some(b) = bb.take() {
        let trimmed = strip_cjk_spaces(text.trim());
        if !trimmed.is_empty() {
            segs.push(Seg {
                text: trimmed,
                bbox: b,
            });
        }
    }
    text.clear();
}

fn lattice_cols_too_coarse(t: &Table, segs: &[Seg], margin: usize, scale: Option<f64>) -> bool {
    let inside: Vec<&Seg> = segs
        .iter()
        .filter(|s| t.bbox.contains_center(&s.bbox))
        .collect();
    if inside.len() < 2 {
        return false;
    }
    let mut hs: Vec<f64> = inside.iter().map(|s| s.bbox.height()).collect();
    hs.sort_by(|a, b| a.total_cmp(b));
    let line_tol = (scale.unwrap_or(hs[hs.len() / 2]) * 0.6).max(0.5);
    let bottoms: Vec<f64> = inside.iter().map(|s| s.bbox.bottom).collect();
    let ids = detect_text::cluster_ids(&bottoms, line_tol);
    let n = ids.iter().copied().max().unwrap_or(0) + 1;
    let mut lines: Vec<Vec<&Seg>> = vec![Vec::new(); n];
    for (k, &id) in ids.iter().enumerate() {
        lines[id].push(inside[k]);
    }
    let need = t.n_cols + margin;
    let rich: Vec<&Vec<&Seg>> = lines.iter().filter(|l| l.len() >= need).collect();
    rich.iter().enumerate().any(|(i, a)| {
        rich[i + 1..]
            .iter()
            .any(|b| aligned_col_count(a, b, 3.0) >= need)
    })
}

fn aligned_col_count(a: &[&Seg], b: &[&Seg], tol: f64) -> usize {
    let xs: Vec<f64> = a
        .iter()
        .filter(|s| {
            b.iter().any(|u| {
                (s.bbox.x0 - u.bbox.x0).abs() <= tol
                    || (s.bbox.x1 - u.bbox.x1).abs() <= tol
                    || ((s.bbox.x0 + s.bbox.x1) - (u.bbox.x0 + u.bbox.x1)).abs() <= 2.0 * tol
            })
        })
        .map(|s| s.bbox.x0)
        .collect();
    if xs.is_empty() {
        return 0;
    }
    let ids = detect_text::cluster_ids(&xs, tol);
    ids.iter().copied().max().unwrap_or(0) + 1
}

/// 数値主体の行か
///
/// 数字を含みアルファベットを含まない行に加え 科学記法のe/Eだけは字として許す
/// 記号（% 桁区切り 括弧 通貨等）は従来通り不問
fn numeric_typed_line(l: &[&Glyph]) -> bool {
    l.iter().any(|g| g.ch.is_ascii_digit())
        && l.iter()
            .all(|g| !g.ch.is_alphabetic() || matches!(g.ch, 'e' | 'E'))
}

/// セル内の1テキスト行と型付き内容の特徴
struct LineMark {
    col: usize,
    top: f64,
    bot: f64,
    typed_evidence: bool,
}

/// 罫線行内を細分する行境界の算出
fn row_split_boundaries(
    row: &[Cell],
    chars: &[Glyph],
    index: &CharIndex,
    scale: Option<f64>,
) -> Vec<f64> {
    let mut marks: Vec<LineMark> = Vec::new();
    let mut heights: Vec<f64> = Vec::new();
    for (c, cell) in row.iter().enumerate() {
        if cell.bbox.width() <= 0.0 || cell.bbox.height() <= 0.0 {
            continue;
        }
        let mut gs: Vec<&Glyph> = index
            .in_box(chars, &cell.bbox)
            .into_iter()
            .map(|i| &chars[i])
            .filter(|g| !g.ch.is_whitespace())
            .collect();
        if gs.is_empty() {
            continue;
        }
        heights.extend(gs.iter().map(|g| g.bbox.height()));
        gs.sort_by(|a, b| a.bbox.bottom.total_cmp(&b.bbox.bottom));
        let med_h = scale.unwrap_or_else(|| {
            let mut hs: Vec<f64> = gs.iter().map(|g| g.bbox.height()).collect();
            hs.sort_by(|a, b| a.total_cmp(b));
            hs[hs.len() / 2]
        });
        let line_tol = (med_h * 0.6).max(0.5);
        let mut lines: Vec<Vec<&Glyph>> = Vec::new();
        let mut cur: Vec<&Glyph> = Vec::new();
        let mut last_b = f64::NEG_INFINITY;
        for g in gs {
            if !cur.is_empty() && g.bbox.bottom - last_b > line_tol {
                lines.push(std::mem::take(&mut cur));
            }
            last_b = g.bbox.bottom;
            cur.push(g);
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
        let typed: Vec<bool> = lines.iter().map(|l| numeric_typed_line(l)).collect();
        let typed_majority = typed.iter().filter(|&&t| t).count() * 2 >= lines.len();
        for (k, l) in lines.iter().enumerate() {
            marks.push(LineMark {
                col: c,
                top: l.iter().map(|g| g.bbox.top).fold(f64::INFINITY, f64::min),
                bot: l
                    .iter()
                    .map(|g| g.bbox.bottom)
                    .fold(f64::NEG_INFINITY, f64::max),
                typed_evidence: typed[k] && typed_majority,
            });
        }
    }
    if marks.len() < 2 {
        return Vec::new();
    }

    heights.sort_by(|a, b| a.total_cmp(b));
    let row_tol = (scale.unwrap_or(heights[heights.len() / 2]) * 0.6).max(0.5);
    let ids = detect_text::cluster_ids(&marks.iter().map(|m| m.bot).collect::<Vec<_>>(), row_tol);
    let n = ids.iter().copied().max().unwrap_or(0) + 1;
    if n < 2 {
        return Vec::new();
    }
    let mut cols: Vec<std::collections::HashSet<usize>> = vec![Default::default(); n];
    let mut top = vec![f64::INFINITY; n];
    let mut bot = vec![f64::NEG_INFINITY; n];
    let mut typed_any = vec![false; n];
    for (k, &rid) in ids.iter().enumerate() {
        cols[rid].insert(marks[k].col);
        top[rid] = top[rid].min(marks[k].top);
        bot[rid] = bot[rid].max(marks[k].bot);
        typed_any[rid] |= marks[k].typed_evidence;
    }
    // 両側とも複列かつ下側に数値型の裏づけがあるときだけ切る
    // 片側だけの複列はラベル折返し＋数値の中間配置と区別できない
    let multi = |i: usize| cols[i].len() >= 2;
    let splits = |i: usize| multi(i) && typed_any[i];

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| top[a].total_cmp(&top[b]));
    let mut out = Vec::new();
    for w in order.windows(2) {
        if multi(w[0]) && splits(w[1]) {
            out.push((bot[w[0]] + top[w[1]]) / 2.0);
        }
    }
    out
}

/// セル内の空白がx境界での分割を許すか
///
/// 境界をまたぐグリフが無く 境界を含む空白が字高スケール比で十分広いときだけ許す
/// 結合セルの跨ぎテキストは境界がグリフに当たるため自然に保護される
fn col_split_allowed(gs: &[&Glyph], x: f64, gap_min: f64) -> bool {
    let mut left = f64::NEG_INFINITY;
    let mut right = f64::INFINITY;
    for g in gs {
        if g.bbox.cx() < x {
            left = left.max(g.bbox.x1);
        } else {
            right = right.min(g.bbox.x0);
        }
    }
    left <= x + 0.5 && right >= x - 0.5 && right - left >= gap_min
}

/// 疎罫線で複数列にまたがるセルを格子の列軸で細分する
fn refine_lattice_cols(
    t: &mut Table,
    chars: &[Glyph],
    index: &CharIndex,
    scale: Option<f64>,
    bidi: bool,
) {
    if t.n_cols < 2 {
        return;
    }
    // 列軸のx座標。非退化セルを持たない列は境界不明として扱う
    let mut xs: Vec<Option<f64>> = vec![None; t.n_cols];
    for row in &t.data {
        for (j, cell) in row.iter().enumerate() {
            if cell.bbox.width() > 0.0 && xs[j].is_none() {
                xs[j] = Some(cell.bbox.x0);
            }
        }
    }

    for row in &mut t.data {
        for j in 0..row.len() {
            let cell = &row[j];
            if cell.bbox.width() <= 0.0 || cell.bbox.height() <= 0.0 {
                continue;
            }
            // このセルが覆う列境界
            let bounds: Vec<(usize, f64)> = (j + 1..t.n_cols)
                .filter_map(|b| xs[b].map(|x| (b, x)))
                .filter(|&(_, x)| x > cell.bbox.x0 + 1.0 && x < cell.bbox.x1 - 1.0)
                .collect();
            if bounds.is_empty() {
                continue;
            }
            let gs: Vec<&Glyph> = index
                .in_box(chars, &cell.bbox)
                .into_iter()
                .map(|i| &chars[i])
                .filter(|g| !g.ch.is_whitespace())
                .collect();
            let gap_min = 0.5
                * scale.unwrap_or_else(|| {
                    let mut hs: Vec<f64> = gs.iter().map(|g| g.bbox.height()).collect();
                    hs.sort_by(|a, b| a.total_cmp(b));
                    hs.get(hs.len() / 2).copied().unwrap_or(0.0)
                });
            let cuts: Vec<(usize, f64)> = bounds
                .into_iter()
                .filter(|&(_, x)| col_split_allowed(&gs, x, gap_min))
                .collect();
            if cuts.is_empty() {
                continue;
            }
            let (top, bottom, x_end) = (cell.bbox.top, cell.bbox.bottom, cell.bbox.x1);
            let mut slot = j;
            let mut x_start = cell.bbox.x0;
            for (b, x) in cuts.into_iter().chain(std::iter::once((t.n_cols, x_end))) {
                let bb = BBox {
                    x0: x_start,
                    top,
                    x1: x,
                    bottom,
                };
                row[slot] = Cell {
                    text: cell_text(chars, index, &bb, bidi),
                    bbox: bb,
                };
                slot = b;
                x_start = x;
            }
        }
    }
}

/// 疎罫線の罫線行内をテキスト行で細分する
fn refine_lattice_rows(
    t: &mut Table,
    chars: &[Glyph],
    index: &CharIndex,
    scale: Option<f64>,
    bidi: bool,
) {
    let mut new_data: Vec<Vec<Cell>> = Vec::with_capacity(t.data.len());
    let mut changed = false;
    for row in &t.data {
        let bounds = row_split_boundaries(row, chars, index, scale);
        if bounds.is_empty() {
            new_data.push(row.clone());
            continue;
        }
        changed = true;
        let row_top = row
            .iter()
            .filter(|c| c.bbox.height() > 0.0)
            .map(|c| c.bbox.top)
            .fold(f64::INFINITY, f64::min);
        let row_bottom = row
            .iter()
            .filter(|c| c.bbox.height() > 0.0)
            .map(|c| c.bbox.bottom)
            .fold(f64::NEG_INFINITY, f64::max);
        let mut ys = Vec::with_capacity(bounds.len() + 2);
        ys.push(row_top);
        ys.extend(bounds);
        ys.push(row_bottom);
        for w in ys.windows(2) {
            let mut new_row = Vec::with_capacity(row.len());
            for cell in row {
                let top = cell.bbox.top.max(w[0]);
                let bottom = cell.bbox.bottom.min(w[1]);
                if cell.bbox.width() > 0.0 && bottom - top > 0.0 {
                    let bb = BBox {
                        x0: cell.bbox.x0,
                        top,
                        x1: cell.bbox.x1,
                        bottom,
                    };
                    new_row.push(Cell {
                        text: cell_text(chars, index, &bb, bidi),
                        bbox: bb,
                    });
                } else {
                    new_row.push(Cell {
                        text: String::new(),
                        bbox: BBox {
                            x0: 0.0,
                            top: 0.0,
                            x1: 0.0,
                            bottom: 0.0,
                        },
                    });
                }
            }
            new_data.push(new_row);
        }
    }
    if changed {
        t.n_rows = new_data.len();
        t.data = new_data;
    }
}

/// 矩形の重なり判定
fn overlaps_any(b: &BBox, tables: &[Table]) -> bool {
    tables.iter().any(|t| {
        let ox0 = b.x0.max(t.bbox.x0);
        let ox1 = b.x1.min(t.bbox.x1);
        let oy0 = b.top.max(t.bbox.top);
        let oy1 = b.bottom.min(t.bbox.bottom);
        let ow = (ox1 - ox0).max(0.0);
        let oh = (oy1 - oy0).max(0.0);
        let oarea = ow * oh;
        let barea = b.width() * b.height();
        barea > 0.0 && oarea / barea > 0.3
    })
}

/// 折返し断片の孤児行を隣接する本行へ吸収する
///
/// 非空が1列だけの連続行を 複列行の空セルへ上下まとめて結合する
fn absorb_wrap_orphan_rows(t: &mut Table) {
    let filled = |row: &[Cell]| -> Vec<usize> {
        row.iter()
            .enumerate()
            .filter(|(_, c)| !c.text.trim().is_empty())
            .map(|(i, _)| i)
            .collect()
    };
    let cell_empty = |row: &[Cell], c: usize| {
        row.get(c)
            .map(|cell| cell.text.trim().is_empty())
            .unwrap_or(true)
    };

    loop {
        let n = t.data.len();
        if n < 2 {
            return;
        }
        // (本行, 上側孤児インデックス上から下, 下側孤児インデックス上から下, 列)
        let mut job: Option<(usize, Vec<usize>, Vec<usize>, usize)> = None;
        for j in 0..n {
            if filled(&t.data[j]).len() < 2 {
                continue;
            }
            let mut above: Vec<usize> = Vec::new();
            let mut col: Option<usize> = None;
            let mut i = j;
            while i > 0 {
                i -= 1;
                let cols = filled(&t.data[i]);
                if cols.len() != 1 {
                    break;
                }
                let c = cols[0];
                if let Some(pc) = col {
                    if pc != c {
                        break;
                    }
                } else if !cell_empty(&t.data[j], c) {
                    break;
                }
                col = Some(c);
                above.push(i);
            }
            above.reverse();
            let mut below: Vec<usize> = Vec::new();
            i = j;
            while i + 1 < n {
                i += 1;
                let cols = filled(&t.data[i]);
                if cols.len() != 1 {
                    break;
                }
                let c = cols[0];
                if let Some(pc) = col {
                    if pc != c {
                        break;
                    }
                } else if !cell_empty(&t.data[j], c) {
                    break;
                }
                col = Some(c);
                below.push(i);
            }
            if let Some(c) = col {
                if !above.is_empty() || !below.is_empty() {
                    job = Some((j, above, below, c));
                    break;
                }
            }
        }
        let Some((ti, above, below, c)) = job else {
            return;
        };
        let mut parts: Vec<(String, BBox)> = Vec::new();
        for &oi in above.iter().chain(below.iter()) {
            parts.push((t.data[oi][c].text.clone(), t.data[oi][c].bbox));
        }
        let existing = t.data[ti][c].text.trim();
        let mut texts: Vec<String> = above.iter().map(|&oi| t.data[oi][c].text.clone()).collect();
        if !existing.is_empty() {
            texts.push(existing.to_string());
        }
        texts.extend(below.iter().map(|&oi| t.data[oi][c].text.clone()));
        let merged = texts.join("\n");
        let mut bb = t.data[ti][c].bbox;
        for (_, ob) in &parts {
            if ob.width() <= 0.0 || ob.height() <= 0.0 {
                continue;
            }
            if bb.width() <= 0.0 || bb.height() <= 0.0 {
                bb = *ob;
            } else {
                bb = BBox {
                    x0: bb.x0.min(ob.x0),
                    top: bb.top.min(ob.top),
                    x1: bb.x1.max(ob.x1),
                    bottom: bb.bottom.max(ob.bottom),
                };
            }
        }
        t.data[ti][c].text = merged;
        t.data[ti][c].bbox = bb;
        let mut drop: Vec<usize> = above.into_iter().chain(below.into_iter()).collect();
        drop.sort_unstable();
        drop.dedup();
        for oi in drop.into_iter().rev() {
            t.data.remove(oi);
        }
        t.n_rows = t.data.len();
    }
}

/// 全行が空の列と全列が空の行を落とす
fn prune_empty(t: &mut Table) {
    let keep_cols: Vec<usize> = (0..t.n_cols)
        .filter(|&c| {
            t.data
                .iter()
                .any(|r| c < r.len() && !r[c].text.trim().is_empty())
        })
        .collect();
    let keep_rows: Vec<usize> = (0..t.data.len())
        .filter(|&r| t.data[r].iter().any(|cell| !cell.text.trim().is_empty()))
        .collect();

    let empty_cell = || Cell {
        text: String::new(),
        bbox: BBox {
            x0: 0.0,
            top: 0.0,
            x1: 0.0,
            bottom: 0.0,
        },
    };
    let data: Vec<Vec<Cell>> = keep_rows
        .iter()
        .map(|&r| {
            keep_cols
                .iter()
                .map(|&c| t.data[r].get(c).cloned().unwrap_or_else(empty_cell))
                .collect()
        })
        .collect();
    t.n_rows = data.len();
    t.n_cols = keep_cols.len();
    t.data = data;

    // 残セルでの bbox 更新
    let mut bb: Option<BBox> = None;
    for c in t.data.iter().flatten() {
        if c.bbox.width() <= 0.0 || c.bbox.height() <= 0.0 {
            continue;
        }
        bb = Some(match bb {
            Some(b) => BBox {
                x0: b.x0.min(c.bbox.x0),
                top: b.top.min(c.bbox.top),
                x1: b.x1.max(c.bbox.x1),
                bottom: b.bottom.max(c.bbox.bottom),
            },
            None => c.bbox,
        });
    }
    if let Some(b) = bb {
        t.bbox = b;
    }
}

/// fs 供給ページの閾値スケール係数（フォントサイズ→行/分割スケール）
const FS_SCALE_LATIN: f64 = 0.85;
const FS_SCALE_CJK: f64 = 1.10;

/// フォントサイズ供給時の閾値スケールを求める
///
/// 過半のグリフが font_size を持つときだけ Some。スケールは
/// fs 中央値×係数で、CJK 優勢ページは全角前提の広い係数を使う
fn fs_threshold_scale(glyphs: &[GlyphPart]) -> Option<f64> {
    let solid: Vec<&GlyphPart> = glyphs.iter().filter(|g| !g.ch.is_whitespace()).collect();
    if solid.is_empty() {
        return None;
    }
    let mut fss: Vec<f64> = solid
        .iter()
        .filter_map(|g| g.font_size)
        .filter(|v| *v > 0.0)
        .collect();
    if fss.len() * 2 < solid.len() {
        return None;
    }
    fss.sort_by(|a, b| a.total_cmp(b));
    let med_fs = fss[fss.len() / 2];
    let cjk = solid.iter().filter(|g| is_cjk(g.ch)).count();
    let coef = if cjk * 10 >= solid.len() * 3 {
        FS_SCALE_CJK
    } else {
        FS_SCALE_LATIN
    };
    Some(med_fs * coef)
}

/// 外部リーダーが取り出した文字と線分からの全ページ表抽出
pub fn extract_from_parts(pages: Vec<PagePart>, options: &ExtractOptions) -> Document {
    let out = crate::par::map_ordered(pages, || (), |_, pi, part| detect_page(pi, part, options));
    Document {
        source: "<parts>".to_string(),
        pages: out,
        warnings: Vec::new(),
    }
}

/// 1ページ分の表検出と出力座標系への復元
fn detect_page(pi: usize, part: PagePart, options: &ExtractOptions) -> Page {
    let scale = fs_threshold_scale(&part.glyphs);
    let to_glyph = |g: &GlyphPart| Glyph {
        ch: g.ch,
        bbox: BBox {
            x0: g.left.min(g.right),
            top: g.top.min(g.bottom),
            x1: g.left.max(g.right),
            bottom: g.top.max(g.bottom),
        },
    };
    let chars: Vec<Glyph> = part
        .glyphs
        .iter()
        .filter(|g| g.upright)
        .map(to_glyph)
        .collect();
    let mut edges: Vec<Edge> = part
        .edges
        .iter()
        .map(|e| Edge {
            x0: e.left.min(e.right),
            top: e.top.min(e.bottom),
            x1: e.left.max(e.right),
            bottom: e.top.max(e.bottom),
            orientation: e.orientation,
        })
        .collect();
    // 字高スケール未満の線分片（グラフの曲線・破線の断片）は罫線候補にしない
    if let Some(s) = scale {
        edges.retain(|e| e.length() >= 0.5 * s);
    }
    let mut tables = detect_tables(&chars, &edges, options, scale);

    // ページ内で部分的に回転している表の検出
    // 同じ回転角のグリフ群を回転フレームへ写して検出し 結果を戻して合流する
    for rot in [90, 180, 270] {
        let group: Vec<&GlyphPart> = part
            .glyphs
            .iter()
            .filter(|g| !g.upright && g.rot == rot)
            .collect();
        if group.iter().filter(|g| !g.ch.is_whitespace()).count() < options.text.min_table_segs {
            continue;
        }
        let (w, h) = (part.width, part.height);
        let chars_r: Vec<Glyph> = group
            .iter()
            .map(|g| {
                let mut gl = to_glyph(g);
                gl.bbox = norm_box(&gl.bbox, rot, w, h);
                gl
            })
            .collect();
        let edges_r: Vec<Edge> = edges
            .iter()
            .map(|e| {
                let bb = norm_box(
                    &BBox {
                        x0: e.x0,
                        top: e.top,
                        x1: e.x1,
                        bottom: e.bottom,
                    },
                    rot,
                    w,
                    h,
                );
                Edge {
                    x0: bb.x0,
                    top: bb.top,
                    x1: bb.x1,
                    bottom: bb.bottom,
                    orientation: if rot == 180 {
                        e.orientation
                    } else {
                        match e.orientation {
                            Orientation::Horizontal => Orientation::Vertical,
                            Orientation::Vertical => Orientation::Horizontal,
                        }
                    },
                }
            })
            .collect();
        let (fw, fh) = if rot == 180 { (w, h) } else { (h, w) };
        for mut t in detect_tables(&chars_r, &edges_r, options, scale) {
            t.bbox = denorm_box(&t.bbox, rot, fw, fh);
            for cell in t.data.iter_mut().flatten() {
                if cell.bbox.width() > 0.0 || cell.bbox.height() > 0.0 {
                    cell.bbox = denorm_box(&cell.bbox, rot, fw, fh);
                }
            }
            if !overlaps_any(&t.bbox, &tables) {
                tables.push(t);
            }
        }
    }
    // チャート領域(曲線・斜め線に覆われた領域)の候補を棄却する
    tables.retain(|t| graphics_coverage(&t.bbox, &part.graphics) < GRAPHICS_COVER_MAX);
    tables.sort_by(|a, b| {
        a.bbox
            .top
            .total_cmp(&b.bbox.top)
            .then_with(|| a.bbox.x0.total_cmp(&b.bbox.x0))
    });

    let (mut width, mut height) = (part.width, part.height);
    if part.norm_rotate != 0 {
        // リーダーの正規化回転を戻し元のページ座標系で出力する
        for t in &mut tables {
            t.bbox = denorm_box(&t.bbox, part.norm_rotate, part.width, part.height);
            for cell in t.data.iter_mut().flatten() {
                if cell.bbox.width() > 0.0 || cell.bbox.height() > 0.0 {
                    cell.bbox = denorm_box(&cell.bbox, part.norm_rotate, part.width, part.height);
                }
            }
        }
        if part.norm_rotate != 180 {
            std::mem::swap(&mut width, &mut height);
        }
    }
    Page {
        page_number: pi + 1,
        width,
        height,
        tables,
    }
}

/// ページ座標を回転フレームへ写す(denorm_boxの逆方向)
///
/// w h は元のページ寸法
fn norm_box(b: &BBox, rot: i32, w: f64, h: f64) -> BBox {
    match rot {
        90 => BBox {
            x0: h - b.bottom,
            top: b.x0,
            x1: h - b.top,
            bottom: b.x1,
        },
        180 => BBox {
            x0: w - b.x1,
            top: h - b.bottom,
            x1: w - b.x0,
            bottom: h - b.top,
        },
        270 => BBox {
            x0: b.top,
            top: w - b.x1,
            x1: b.bottom,
            bottom: w - b.x0,
        },
        _ => *b,
    }
}

/// グラフィック被覆率がこの値以上の表候補はチャート領域として棄却する
const GRAPHICS_COVER_MAX: f64 = 0.5;

/// 表bboxのうちグラフィック矩形に覆われる割合
///
/// 20x20の格子サンプリングで近似する
fn graphics_coverage(b: &BBox, graphics: &[GraphicPart]) -> f64 {
    if graphics.is_empty() || b.width() <= 0.0 || b.height() <= 0.0 {
        return 0.0;
    }
    let n = 20;
    let mut hit = 0;
    for i in 0..n {
        for j in 0..n {
            let x = b.x0 + (i as f64 + 0.5) / n as f64 * b.width();
            let y = b.top + (j as f64 + 0.5) / n as f64 * b.height();
            let inside = graphics.iter().any(|g| {
                g.left.min(g.right) <= x
                    && x <= g.left.max(g.right)
                    && g.top.min(g.bottom) <= y
                    && y <= g.top.max(g.bottom)
            });
            if inside {
                hit += 1;
            }
        }
    }
    hit as f64 / (n * n) as f64
}

/// 正規化回転された座標を元のページ座標へ戻す
///
/// w h は正規化後のページ寸法
fn denorm_box(b: &BBox, rot: i32, w: f64, h: f64) -> BBox {
    match rot {
        90 => BBox {
            x0: b.top,
            top: w - b.x1,
            x1: b.bottom,
            bottom: w - b.x0,
        },
        180 => BBox {
            x0: w - b.x1,
            top: h - b.bottom,
            x1: w - b.x0,
            bottom: h - b.top,
        },
        270 => BBox {
            x0: h - b.bottom,
            top: b.x0,
            x1: h - b.top,
            bottom: b.x1,
        },
        _ => *b,
    }
}

/// 3方式の検出を優先順で実行し1ページ分の表を確定する
fn detect_tables(
    chars: &[Glyph],
    edges: &[Edge],
    options: &ExtractOptions,
    scale: Option<f64>,
) -> Vec<Table> {
    let index = CharIndex::new(chars);
    let bidi = options.bidi;
    let fill = |b: &BBox| cell_text(chars, &index, b, bidi);
    let use_lattice = options.mode != DetectMode::Borderless;
    let use_hybrid = options.mode == DetectMode::Auto;
    let use_text = options.mode != DetectMode::Ruled;
    let mut tables = if use_lattice {
        detect_lattice::detect(edges, &fill, &options.lattice)
    } else {
        Vec::new()
    };
    let all_segs = build_segments(chars, scale);
    if options.lattice.col_check && (use_hybrid || use_text) {
        tables.retain(|t| {
            !lattice_cols_too_coarse(t, &all_segs, options.lattice.col_check_margin, scale)
        });
    }
    if options.lattice.refine_cols {
        for t in &mut tables {
            refine_lattice_cols(t, chars, &index, scale, bidi);
        }
    }
    if options.lattice.refine_rows {
        for t in &mut tables {
            refine_lattice_rows(t, chars, &index, scale, bidi);
        }
    }
    for t in &mut tables {
        absorb_wrap_orphan_rows(t);
        prune_empty(t);
    }
    tables.retain(|t| t.n_rows >= options.lattice.min_rows && t.n_cols >= options.lattice.min_cols);

    // 確定したlattice表の領域内の文字は後続の検出の入力から除く
    let segs: Vec<Seg> = all_segs
        .into_iter()
        .filter(|seg| !tables.iter().any(|t| t.bbox.contains_center(&seg.bbox)))
        .collect();

    let mut hybrid_tables = if use_hybrid {
        detect_hybrid::detect(&segs, edges, &fill, &options.hybrid, scale)
    } else {
        Vec::new()
    };
    for t in &mut hybrid_tables {
        absorb_wrap_orphan_rows(t);
        prune_empty(t);
    }
    for t in hybrid_tables {
        if t.n_rows >= options.hybrid.min_rows
            && t.n_cols >= options.hybrid.min_cols
            && !overlaps_any(&t.bbox, &tables)
        {
            tables.push(t);
        }
    }

    // 確定したhybrid表の領域内の文字も除いてからtext検出に渡す
    let segs: Vec<Seg> = segs
        .into_iter()
        .filter(|seg| !tables.iter().any(|t| t.bbox.contains_center(&seg.bbox)))
        .collect();
    let mut text_tables = if use_text {
        detect_text::detect(&segs, &options.text, scale)
    } else {
        Vec::new()
    };
    for t in &mut text_tables {
        absorb_wrap_orphan_rows(t);
        prune_empty(t);
    }
    for t in text_tables {
        if t.n_rows >= options.text.min_rows
            && t.n_cols >= options.text.min_cols
            && !overlaps_any(&t.bbox, &tables)
        {
            tables.push(t);
        }
    }

    // ページ内の読み順に揃える
    tables.sort_by(|a, b| {
        a.bbox
            .top
            .total_cmp(&b.bbox.top)
            .then_with(|| a.bbox.x0.total_cmp(&b.bbox.x0))
    });
    tables
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(ch: char, x0: f64, x1: f64, top: f64, bottom: f64) -> Glyph {
        Glyph {
            ch,
            bbox: BBox {
                x0,
                top,
                x1,
                bottom,
            },
        }
    }

    fn cell(x0: f64, top: f64, x1: f64, bottom: f64, text: &str) -> Cell {
        Cell {
            text: text.to_string(),
            bbox: BBox {
                x0,
                top,
                x1,
                bottom,
            },
        }
    }

    fn empty_cell() -> Cell {
        cell(0.0, 0.0, 0.0, 0.0, "")
    }

    /// ヘッダー行だけ2列に割れ 本体行が全幅1セルの格子
    fn header_only_grid() -> Table {
        Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 100.0,
                bottom: 20.0,
            },
            n_rows: 2,
            n_cols: 2,
            data: vec![
                vec![
                    cell(0.0, 0.0, 50.0, 10.0, "h1"),
                    cell(50.0, 0.0, 100.0, 10.0, "h2"),
                ],
                vec![cell(0.0, 10.0, 100.0, 20.0, "a b"), empty_cell()],
            ],
        }
    }

    #[test]
    fn graphics_coverage_counts_sampled_area() {
        let b = BBox {
            x0: 0.0,
            top: 0.0,
            x1: 100.0,
            bottom: 100.0,
        };
        let full = vec![GraphicPart {
            left: -1.0,
            right: 101.0,
            top: -1.0,
            bottom: 101.0,
        }];
        assert_eq!(graphics_coverage(&b, &full), 1.0);
        let strip = vec![GraphicPart {
            left: 0.0,
            right: 20.0,
            top: 0.0,
            bottom: 100.0,
        }];
        assert_eq!(graphics_coverage(&b, &strip), 0.2);
        assert_eq!(graphics_coverage(&b, &[]), 0.0);
    }

    #[test]
    fn norm_box_roundtrips_with_denorm() {
        let b = BBox {
            x0: 10.0,
            top: 20.0,
            x1: 30.0,
            bottom: 40.0,
        };
        for rot in [90, 180, 270] {
            let (w, h) = (100.0, 200.0);
            let n = norm_box(&b, rot, w, h);
            let (fw, fh) = if rot == 180 { (w, h) } else { (h, w) };
            let o = denorm_box(&n, rot, fw, fh);
            assert_eq!((o.x0, o.top, o.x1, o.bottom), (10.0, 20.0, 30.0, 40.0));
        }
    }

    #[test]
    fn denorm_box_90_restores_original() {
        // 元100x200のページを90度正規化した200x100上の箱を戻す
        let b = BBox {
            x0: 160.0,
            top: 10.0,
            x1: 180.0,
            bottom: 30.0,
        };
        let o = denorm_box(&b, 90, 200.0, 100.0);
        assert_eq!((o.x0, o.top, o.x1, o.bottom), (10.0, 20.0, 30.0, 40.0));
    }

    #[test]
    fn denorm_box_270_restores_original() {
        let b = BBox {
            x0: 20.0,
            top: 70.0,
            x1: 40.0,
            bottom: 90.0,
        };
        let o = denorm_box(&b, 270, 200.0, 100.0);
        assert_eq!((o.x0, o.top, o.x1, o.bottom), (10.0, 20.0, 30.0, 40.0));
    }

    #[test]
    fn refine_cols_splits_full_width_cell_at_column_axis() {
        let mut t = header_only_grid();
        let chars = vec![
            glyph('a', 5.0, 10.0, 12.0, 18.0),
            glyph('b', 55.0, 60.0, 12.0, 18.0),
        ];
        refine_lattice_cols(&mut t, &chars, &CharIndex::new(&chars), None, false);
        assert_eq!(t.data[1][0].text, "a");
        assert_eq!(t.data[1][1].text, "b");
        assert_eq!(t.data[1][0].bbox.x1, 50.0);
        assert_eq!(t.data[1][1].bbox.x0, 50.0);
    }

    #[test]
    fn refine_cols_keeps_cell_when_glyph_straddles_axis() {
        let mut t = header_only_grid();
        let chars = vec![
            glyph('a', 5.0, 10.0, 12.0, 18.0),
            glyph('c', 45.0, 55.0, 12.0, 18.0),
        ];
        refine_lattice_cols(&mut t, &chars, &CharIndex::new(&chars), None, false);
        assert_eq!(t.data[1][0].bbox.x1, 100.0);
        assert_eq!(t.data[1][1].bbox.width(), 0.0);
    }

    #[test]
    fn refine_cols_keeps_cell_when_gap_is_too_narrow() {
        let mut t = header_only_grid();
        let chars = vec![
            glyph('a', 5.0, 49.0, 12.0, 18.0),
            glyph('b', 51.0, 95.0, 12.0, 18.0),
        ];
        refine_lattice_cols(&mut t, &chars, &CharIndex::new(&chars), None, false);
        assert_eq!(t.data[1][0].bbox.x1, 100.0);
    }

    /// 1罫線行に複数セルを横に並べ 各セルに2テキスト行を入れる
    fn one_band_table(n_cells: usize) -> Table {
        let w = 50.0;
        let row: Vec<Cell> = (0..n_cells)
            .map(|j| cell(w * j as f64, 0.0, w * (j + 1) as f64, 30.0, ""))
            .collect();
        Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: w * n_cells as f64,
                bottom: 30.0,
            },
            n_rows: 1,
            n_cols: n_cells,
            data: vec![row],
        }
    }

    fn two_lines(chars: &mut Vec<Glyph>, cell_idx: usize, line1: &str, line2: &str) {
        for (k, s) in [line1, line2].iter().enumerate() {
            let top = 2.0 + 10.0 * k as f64;
            for (i, ch) in s.chars().enumerate() {
                let x = 50.0 * cell_idx as f64 + 5.0 + 4.0 * i as f64;
                chars.push(glyph(ch, x, x + 3.0, top, top + 6.0));
            }
        }
    }

    #[test]
    fn refine_rows_splits_on_scientific_notation() {
        let mut t = one_band_table(2);
        let mut chars = Vec::new();
        two_lines(&mut chars, 0, "ab", "cd");
        two_lines(&mut chars, 1, "1E5", "2E6");
        refine_lattice_rows(&mut t, &chars, &CharIndex::new(&chars), None, false);
        assert_eq!(t.n_rows, 2);
        assert_eq!(t.data[0][1].text, "1E5");
        assert_eq!(t.data[1][1].text, "2E6");
    }

    #[test]
    fn refine_rows_keeps_text_only_lines() {
        // 文字だけの行クラスタは折返しと区別できないため列数に関わらず割らない
        for n_cells in [2, 3] {
            let mut t = one_band_table(n_cells);
            let mut chars = Vec::new();
            for j in 0..n_cells {
                two_lines(&mut chars, j, "ab", "cd");
            }
            refine_lattice_rows(&mut t, &chars, &CharIndex::new(&chars), None, false);
            assert_eq!(t.n_rows, 1);
        }
    }

    #[test]
    fn refine_rows_keeps_single_column_wrap_with_mid_numbers() {
        // 先頭列だけ2行折返し・他列は中間高さの1行数値 → 1論理行のまま
        let mut t = one_band_table(4);
        let mut chars = Vec::new();
        two_lines(&mut chars, 0, "ab", "cd");
        for (j, s) in [(1usize, "12"), (2, "34"), (3, "56")] {
            for (i, ch) in s.chars().enumerate() {
                let x = 50.0 * j as f64 + 5.0 + 4.0 * i as f64;
                chars.push(glyph(ch, x, x + 3.0, 7.0, 13.0));
            }
        }
        refine_lattice_rows(&mut t, &chars, &CharIndex::new(&chars), None, false);
        assert_eq!(t.n_rows, 1);
        assert_eq!(t.n_cols, 4);
    }

    #[test]
    fn absorb_orphan_merges_wrap_fragments_into_value_row() {
        let mut t = Table {
            extraction_method: "text",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 120.0,
                bottom: 30.0,
            },
            n_rows: 3,
            n_cols: 3,
            data: vec![
                vec![
                    cell(0.0, 0.0, 40.0, 10.0, "みか"),
                    empty_cell(),
                    empty_cell(),
                ],
                vec![
                    empty_cell(),
                    cell(40.0, 10.0, 80.0, 20.0, "85"),
                    cell(80.0, 10.0, 120.0, 20.0, "980"),
                ],
                vec![
                    cell(0.0, 20.0, 40.0, 30.0, "ん"),
                    empty_cell(),
                    empty_cell(),
                ],
            ],
        };
        absorb_wrap_orphan_rows(&mut t);
        assert_eq!(t.n_rows, 1);
        assert_eq!(t.data[0][0].text, "みか\nん");
        assert_eq!(t.data[0][1].text, "85");
        assert_eq!(t.data[0][2].text, "980");
    }

    #[test]
    fn refine_cols_spans_only_failed_boundary() {
        let mut t = Table {
            extraction_method: "lattice",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 150.0,
                bottom: 20.0,
            },
            n_rows: 2,
            n_cols: 3,
            data: vec![
                vec![
                    cell(0.0, 0.0, 50.0, 10.0, "h1"),
                    cell(50.0, 0.0, 100.0, 10.0, "h2"),
                    cell(100.0, 0.0, 150.0, 10.0, "h3"),
                ],
                vec![
                    cell(0.0, 10.0, 150.0, 20.0, "a bc"),
                    empty_cell(),
                    empty_cell(),
                ],
            ],
        };
        let chars = vec![
            glyph('a', 5.0, 10.0, 12.0, 18.0),
            glyph('b', 95.0, 99.0, 12.0, 18.0),
            glyph('c', 101.0, 110.0, 12.0, 18.0),
        ];
        refine_lattice_cols(&mut t, &chars, &CharIndex::new(&chars), None, false);
        assert_eq!(t.data[1][0].text, "a");
        assert_eq!(t.data[1][0].bbox.x1, 50.0);
        assert_eq!(t.data[1][1].text, "bc");
        assert_eq!(t.data[1][1].bbox.x1, 150.0);
        assert_eq!(t.data[1][2].bbox.width(), 0.0);
    }
}
