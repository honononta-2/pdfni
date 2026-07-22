//! コンテンツストリーム解釈（パス → edges、テキスト → glyphs）
//!
//! 参考: pdf.js `evaluator.js`

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::extract::{EdgePart, GlyphPart, GraphicPart};
use crate::model::Orientation;
use crate::text::{TextChar, TextFont};

use super::filters::decode_stream;
use super::font::{LoadedFont, lookup_font};
use super::lexer::{Cmd, Lexer, Op, Token};
use super::object::{Dict, Object, Ref, Stream};
use super::parser::Resolver;
use super::xref::XRef;

/// 軸平行判定の許容差（pt）
const EPS: f64 = 0.8;

/// graphics 登録の対象とする非軸平行セグメント弦長の全長比の下限
/// 角丸矩形の四隅や塗り罫線の短辺とチャート要素の区別
const GRAPHICS_MIN_FRAC: f64 = 0.25;

/// Form XObject のネスト上限。正当な入力を弾かない上限
const MAX_FORM_DEPTH: usize = 20;

/// q/Q グラフィック状態スタックの深さ上限
/// Adobe PDF Reference の実装限界 28 に十分な余裕を持たせた値
const MAX_GSTACK_DEPTH: usize = 128;

/// Tf 名前 → フォントの memo エントリ数上限
/// 正当な文書のフォント辞書規模を大きく上回る余裕値
const MAX_FONT_MEMO_ENTRIES: usize = 256;

/// Form 解決用の文脈（xref が無い単体テストでは None）
pub(crate) struct ContentCtx<'a> {
    pub data: &'a [u8],
    pub xref: Option<&'a XRef>,
}

/// テキストAPI用の収集バッファ
struct TextSink {
    fonts: Vec<TextFont>,
    font_ids: HashMap<u64, u32>,
    chars: Vec<PendingTextChar>,
}

/// 回転適用前の座標付き文字
struct PendingTextChar {
    text: String,
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    transform: [f64; 6],
    advance: [f64; 2],
    /// 公称字送り長（Tc・Tw・TJ なし）
    glyph_width: Option<f64>,
    font: u32,
    font_size: f64,
    rot_q: RotQ,
    upright: bool,
    synthetic: bool,
}

/// 進行方向の量子化回転
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RotQ {
    R0,
    R90,
    R180,
    R270,
    /// 90度刻みでない軸外
    OffAxis,
}

impl RotQ {
    /// 90度単位の回転数。軸外は None
    fn quarter(self) -> Option<u8> {
        match self {
            RotQ::R0 => Some(0),
            RotQ::R90 => Some(1),
            RotQ::R180 => Some(2),
            RotQ::R270 => Some(3),
            RotQ::OffAxis => None,
        }
    }

    fn from_quarter(q: u8) -> Self {
        match q % 4 {
            0 => RotQ::R0,
            1 => RotQ::R90,
            2 => RotQ::R180,
            _ => RotQ::R270,
        }
    }

    /// ページ回転 rot(0..=3) を打ち消した値。軸外はそのまま
    fn unrotate(self, rot: u8) -> Self {
        match self.quarter() {
            Some(q) => Self::from_quarter((q + 4 - rot % 4) % 4),
            None => self,
        }
    }

    fn is_upright(self) -> bool {
        self == RotQ::R0
    }

    /// 出力用の角度。軸外は 0
    fn degrees(self) -> i32 {
        self.quarter().map_or(0, |q| i32::from(q) * 90)
    }
}

impl TextSink {
    fn new() -> Self {
        Self {
            fonts: Vec::new(),
            font_ids: HashMap::new(),
            chars: Vec::new(),
        }
    }

    fn font_index(&mut self, font: &LoadedFont, resource_name: &str) -> u32 {
        let key = font.uid();
        if let Some(&i) = self.font_ids.get(&key) {
            return i;
        }
        let i = self.fonts.len() as u32;
        self.fonts.push(TextFont {
            name: font.text_font_name(resource_name).to_string(),
            ascent: font.ascent(),
            descent: font.descent(),
            vertical: font.vertical(),
            bold: font.bold(),
            italic: font.italic(),
        });
        self.font_ids.insert(key, i);
        i
    }
}

/// ページ1枚分の抽出結果（表検出用 parts）
pub(crate) struct ExtractedParts {
    pub glyphs: Vec<GlyphPart>,
    pub edges: Vec<EdgePart>,
    pub graphics: Vec<GraphicPart>,
    /// ページ内容の支配回転を正規化した角度(0/90/180/270)
    pub norm_rotate: i32,
}

/// ページ1枚分の抽出結果（座標付き文字）
pub(crate) struct ExtractedText {
    pub width: f64,
    pub height: f64,
    pub fonts: Vec<TextFont>,
    pub chars: Vec<TextChar>,
}

/// page_rotate はページ属性 /Rotate。出力は表示座標系へ回転済み
pub(crate) fn extract_parts(
    contents: &[u8],
    resources: &Dict,
    view: [f64; 4],
    ctx: ContentCtx<'_>,
    page_rotate: i32,
) -> ExtractedParts {
    let mut tagged = Vec::new();
    let mut edges = Vec::new();
    let mut graphics = Vec::new();
    let mut visiting = HashSet::new();
    Evaluator {
        ctx: &ctx,
        view,
        glyphs: &mut tagged,
        edges: &mut edges,
        graphics: &mut graphics,
        visiting: &mut visiting,
        form_depth: 0,
        text_sink: None,
        collect_paths: true,
        collect_glyphs: true,
    }
    .interpret(contents, resources, IDENTITY_MAT);

    finalize_parts(tagged, edges, graphics, view, page_rotate)
}

fn finalize_parts(
    mut tagged: Vec<(GlyphPart, RotQ)>,
    mut edges: Vec<EdgePart>,
    mut graphics: Vec<GraphicPart>,
    view: [f64; 4],
    page_rotate: i32,
) -> ExtractedParts {
    let (mut w, mut h) = (view[2] - view[0], view[3] - view[1]);

    let page_rot = (page_rotate.rem_euclid(360) / 90) as u8;
    if page_rot != 0 {
        rotate_parts(&mut tagged, &mut edges, &mut graphics, w, h, page_rot);
        for (g, r) in &mut tagged {
            *r = r.unrotate(page_rot);
            g.upright = r.is_upright();
        }
        if page_rot != 2 {
            std::mem::swap(&mut w, &mut h);
        }
    }

    let rot = dominant_rotation(&tagged);
    if rot != 0 {
        rotate_parts(&mut tagged, &mut edges, &mut graphics, w, h, rot);
        for (g, r) in &mut tagged {
            *r = r.unrotate(rot);
            g.upright = r.is_upright();
        }
    }
    let mut glyphs: Vec<GlyphPart> = tagged
        .into_iter()
        .map(|(mut g, r)| {
            g.rot = r.degrees();
            g
        })
        .collect();
    dedup_overlaid_parts(&mut glyphs);
    let spaces = synth_word_spaces(&glyphs);
    glyphs.extend(spaces);
    ExtractedParts {
        glyphs,
        edges,
        graphics,
        norm_rotate: i32::from(rot) * 90,
    }
}

/// ページ /Rotate のみ適用。支配回転の正規化なし
pub(crate) fn extract_text(
    contents: &[u8],
    resources: &Dict,
    view: [f64; 4],
    ctx: ContentCtx<'_>,
    page_rotate: i32,
) -> ExtractedText {
    let mut tagged = Vec::new();
    let mut edges = Vec::new();
    let mut graphics = Vec::new();
    let mut visiting = HashSet::new();
    let mut sink = TextSink::new();
    Evaluator {
        ctx: &ctx,
        view,
        glyphs: &mut tagged,
        edges: &mut edges,
        graphics: &mut graphics,
        visiting: &mut visiting,
        form_depth: 0,
        text_sink: Some(&mut sink),
        collect_paths: false,
        collect_glyphs: false,
    }
    .interpret(contents, resources, IDENTITY_MAT);

    finalize_text(sink, view, page_rotate)
}

/// interpret 共有の 1 パス版。後段の rotate と正規化は parts/text で独立
pub(crate) fn extract_parts_and_text(
    contents: &[u8],
    resources: &Dict,
    view: [f64; 4],
    ctx: ContentCtx<'_>,
    page_rotate: i32,
) -> (ExtractedParts, ExtractedText) {
    let mut tagged = Vec::new();
    let mut edges = Vec::new();
    let mut graphics = Vec::new();
    let mut visiting = HashSet::new();
    let mut sink = TextSink::new();
    Evaluator {
        ctx: &ctx,
        view,
        glyphs: &mut tagged,
        edges: &mut edges,
        graphics: &mut graphics,
        visiting: &mut visiting,
        form_depth: 0,
        text_sink: Some(&mut sink),
        collect_paths: true,
        collect_glyphs: true,
    }
    .interpret(contents, resources, IDENTITY_MAT);

    let parts = finalize_parts(tagged, edges, graphics, view, page_rotate);
    let text = finalize_text(sink, view, page_rotate);
    (parts, text)
}

fn finalize_text(mut sink: TextSink, view: [f64; 4], page_rotate: i32) -> ExtractedText {
    let (mut w, mut h) = (view[2] - view[0], view[3] - view[1]);
    dedup_overlaid_glyphs(&mut sink.chars);
    synth_word_spaces_text(&mut sink.chars);

    let page_rot = (page_rotate.rem_euclid(360) / 90) as u8;
    if page_rot != 0 {
        rotate_text_chars(&mut sink.chars, w, h, page_rot);
        for c in &mut sink.chars {
            c.rot_q = c.rot_q.unrotate(page_rot);
            c.upright = c.rot_q.is_upright();
        }
        if page_rot != 2 {
            std::mem::swap(&mut w, &mut h);
        }
    }

    let chars = sink
        .chars
        .into_iter()
        .map(|c| TextChar {
            text: c.text,
            left: c.left,
            right: c.right,
            top: c.top,
            bottom: c.bottom,
            transform: c.transform,
            advance: c.advance,
            glyph_width: c.glyph_width,
            font: c.font,
            font_size: c.font_size,
            rot: c.rot_q.degrees(),
            upright: c.upright,
            synthetic: c.synthetic,
        })
        .collect();
    ExtractedText {
        width: w,
        height: h,
        fonts: sink.fonts,
        chars,
    }
}

fn rotate_text_chars(chars: &mut [PendingTextChar], w: f64, h: f64, rot: u8) {
    let rmat = page_rot_mat(w, h, rot);
    for c in chars.iter_mut() {
        let rect = Rect {
            left: c.left,
            top: c.top,
            right: c.right,
            bottom: c.bottom,
        }
        .rotate_td(w, h, rot);
        c.left = rect.left;
        c.top = rect.top;
        c.right = rect.right;
        c.bottom = rect.bottom;
        c.transform = mat_mul(rmat, c.transform);
        c.advance = rotate_vec_td(c.advance[0], c.advance[1], rot);
    }
}

/// ページ回転の表示座標アフィン
fn page_rot_mat(w: f64, h: f64, rot: u8) -> [f64; 6] {
    match rot {
        1 => [0.0, 1.0, -1.0, 0.0, h, 0.0],
        2 => [-1.0, 0.0, 0.0, -1.0, w, h],
        3 => [0.0, -1.0, 1.0, 0.0, 0.0, w],
        _ => identity(),
    }
}

fn rotate_vec_td(dx: f64, dy: f64, rot: u8) -> [f64; 2] {
    match rot {
        1 => [-dy, dx],
        2 => [-dx, -dy],
        3 => [dy, -dx],
        _ => [dx, dy],
    }
}

fn rotate_parts(
    tagged: &mut [(GlyphPart, RotQ)],
    edges: &mut [EdgePart],
    graphics: &mut [GraphicPart],
    w: f64,
    h: f64,
    rot: u8,
) {
    for (g, _) in tagged.iter_mut() {
        let rect = Rect {
            left: g.left,
            top: g.top,
            right: g.right,
            bottom: g.bottom,
        }
        .rotate_td(w, h, rot);
        g.left = rect.left;
        g.top = rect.top;
        g.right = rect.right;
        g.bottom = rect.bottom;
    }
    for e in edges.iter_mut() {
        let rect = Rect {
            left: e.left,
            top: e.top,
            right: e.right,
            bottom: e.bottom,
        }
        .rotate_td(w, h, rot);
        e.left = rect.left;
        e.top = rect.top;
        e.right = rect.right;
        e.bottom = rect.bottom;
        if rot != 2 {
            e.orientation = match e.orientation {
                Orientation::Horizontal => Orientation::Vertical,
                Orientation::Vertical => Orientation::Horizontal,
            };
        }
    }
    for g in graphics.iter_mut() {
        let rect = Rect {
            left: g.left,
            top: g.top,
            right: g.right,
            bottom: g.bottom,
        }
        .rotate_td(w, h, rot);
        g.left = rect.left;
        g.top = rect.top;
        g.right = rect.right;
        g.bottom = rect.bottom;
    }
}

/// ページ内容の支配回転の判定に必要な非空白グリフの割合
const NORM_ROT_MIN_RATIO: f64 = 0.9;

/// 返り値は量子化回転 1/2/3 = 90/180/270度。支配回転が無ければ 0
fn dominant_rotation(tagged: &[(GlyphPart, RotQ)]) -> u8 {
    let mut counts = [0usize; 4];
    let mut total = 0usize;
    for (g, r) in tagged {
        if g.ch.is_whitespace() {
            continue;
        }
        total += 1;
        if let Some(q) = r.quarter() {
            counts[q as usize] += 1;
        }
    }
    if total == 0 {
        return 0;
    }
    (1..4)
        .find(|&i| counts[i] as f64 >= NORM_ROT_MIN_RATIO * total as f64)
        .unwrap_or(0) as u8
}

/// 量子化の基準は PDF 空間の反時計回り
fn quantize_rot(ux: f64, uy: f64) -> RotQ {
    if uy.abs() <= 0.05 && ux > 0.05 {
        RotQ::R0
    } else if ux.abs() <= 0.05 && uy > 0.05 {
        RotQ::R90
    } else if uy.abs() <= 0.05 && ux < -0.05 {
        RotQ::R180
    } else if ux.abs() <= 0.05 && uy < -0.05 {
        RotQ::R270
    } else {
        RotQ::OffAxis
    }
}

/// top-down の軸平行矩形
#[derive(Clone, Copy)]
struct Rect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Rect {
    fn rotate_td(self, w: f64, h: f64, rot: u8) -> Rect {
        match rot {
            1 => Rect {
                left: h - self.bottom,
                top: self.left,
                right: h - self.top,
                bottom: self.right,
            },
            2 => Rect {
                left: w - self.right,
                top: h - self.bottom,
                right: w - self.left,
                bottom: h - self.top,
            },
            3 => Rect {
                left: self.top,
                top: w - self.right,
                right: self.bottom,
                bottom: w - self.left,
            },
            _ => self,
        }
    }
}

/// 語間ギャップへの空白グリフ合成
///
/// 検出コアが文章判定に使う語間空白が show 列に無いため
/// 字高 0.12〜1.0 倍のギャップを空白1文字で補完
fn synth_word_spaces(glyphs: &[GlyphPart]) -> Vec<GlyphPart> {
    let mut hs: Vec<f64> = glyphs
        .iter()
        .filter(|g| !g.ch.is_whitespace())
        .map(|g| g.bottom - g.top)
        .collect();
    if hs.len() < 2 {
        return Vec::new();
    }
    hs.sort_by(|a, b| a.total_cmp(b));
    let med_h = hs[hs.len() / 2];
    let line_tol = (med_h * 0.6).max(0.5);

    // 実在の空白も行クラスタへ含めた重複合成の防止
    let mut all: Vec<&GlyphPart> = glyphs.iter().collect();
    all.sort_by(|a, b| a.bottom.total_cmp(&b.bottom));
    let mut lines: Vec<Vec<&GlyphPart>> = Vec::new();
    let mut cur: Vec<&GlyphPart> = Vec::new();
    let mut last_b = f64::NEG_INFINITY;
    for g in all {
        if !cur.is_empty() && g.bottom - last_b > line_tol {
            lines.push(std::mem::take(&mut cur));
        }
        last_b = g.bottom;
        cur.push(g);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    let mut spaces = Vec::new();
    for mut line in lines {
        line.sort_by(|a, b| a.left.total_cmp(&b.left));
        let mut prev_solid: Option<&GlyphPart> = None;
        let mut space_between = false;
        for g in line {
            if g.ch.is_whitespace() {
                space_between = true;
                continue;
            }
            if let Some(a) = prev_solid {
                let gap = g.left - a.right;
                let h = a.bottom - a.top;
                if !space_between && gap > 0.12 * h && gap < 1.0 * h {
                    spaces.push(GlyphPart {
                        ch: ' ',
                        left: a.right,
                        right: g.left,
                        top: a.top,
                        bottom: a.bottom,
                        font_size: a.font_size,
                        upright: a.upright,
                        rot: a.rot,
                    });
                }
            }
            prev_solid = Some(g);
            space_between = false;
        }
    }
    spaces
}

/// 重複グリフ除去の完全一致キー
///
/// 浮動小数13値は -0.0 を +0.0 へ正規化したビット表現
/// text は文字列を所有しない SipHash 値のみ
#[derive(PartialEq, Eq, Hash)]
struct GlyphDedupKey {
    text_hash: u64,
    font: u32,
    rot_q: RotQ,
    upright: bool,
    bits: [u64; 13],
}

fn norm_f64_bits(v: f64) -> u64 {
    if v == 0.0 {
        0.0f64.to_bits()
    } else {
        v.to_bits()
    }
}

/// HashSet 標準と同一の SipHash-1-3 による u64 化
fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// 完全一致の再描画はストリーム先勝ちで1つ。合成空白の生成より前段
fn dedup_overlaid_glyphs(chars: &mut Vec<PendingTextChar>) {
    let mut seen = HashSet::new();
    chars.retain(|c| {
        if c.synthetic || c.text.is_empty() {
            return true;
        }
        let vals = [
            c.font_size,
            c.left,
            c.right,
            c.top,
            c.bottom,
            c.transform[0],
            c.transform[1],
            c.transform[2],
            c.transform[3],
            c.transform[4],
            c.transform[5],
            c.advance[0],
            c.advance[1],
        ];
        if vals.iter().any(|v| !v.is_finite()) {
            return true;
        }
        let bits: [u64; 13] = vals.map(norm_f64_bits);
        seen.insert(GlyphDedupKey {
            text_hash: hash_str(&c.text),
            font: c.font,
            rot_q: c.rot_q,
            upright: c.upright,
            bits,
        })
    });
}

/// 重複グリフ除去の完全一致キー（parts 側）
#[derive(PartialEq, Eq, Hash)]
struct PartDedupKey {
    ch: char,
    upright: bool,
    rot: i32,
    fs_bits: Option<u64>,
    bits: [u64; 4],
}

/// 完全一致の再描画はストリーム先勝ちで1つ。合成空白の生成より前段
fn dedup_overlaid_parts(glyphs: &mut Vec<GlyphPart>) {
    let mut seen = HashSet::new();
    glyphs.retain(|g| {
        let vals = [g.left, g.right, g.top, g.bottom];
        if vals.iter().any(|v| !v.is_finite()) || g.font_size.is_some_and(|v| !v.is_finite()) {
            return true;
        }
        seen.insert(PartDedupKey {
            ch: g.ch,
            upright: g.upright,
            rot: g.rot,
            fs_bits: g.font_size.map(norm_f64_bits),
            bits: vals.map(norm_f64_bits),
        })
    });
}

/// 挿入位置はストリーム順
fn synth_word_spaces_text(chars: &mut Vec<PendingTextChar>) {
    if chars.len() < 2 {
        return;
    }
    let mut hs: Vec<f64> = chars
        .iter()
        .filter(|c| !c.synthetic && !text_is_whitespace(&c.text))
        .map(|c| c.bottom - c.top)
        .collect();
    if hs.len() < 2 {
        return;
    }
    hs.sort_by(|a, b| a.total_cmp(b));
    let med_h = hs[hs.len() / 2];
    let line_tol = (med_h * 0.6).max(0.5);

    let mut by_bottom: Vec<usize> = (0..chars.len()).collect();
    by_bottom.sort_by(|&a, &b| chars[a].bottom.total_cmp(&chars[b].bottom));

    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut last_b = f64::NEG_INFINITY;
    for i in by_bottom {
        let b = chars[i].bottom;
        if !cur.is_empty() && b - last_b > line_tol {
            lines.push(std::mem::take(&mut cur));
        }
        last_b = b;
        cur.push(i);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    // (挿入位置=直前グリフの直後 index+1, 空白)
    let mut inserts: Vec<(usize, PendingTextChar)> = Vec::new();
    for mut line in lines {
        line.sort_by(|&a, &b| chars[a].left.total_cmp(&chars[b].left));
        let mut prev_solid: Option<usize> = None;
        let mut space_between = false;
        for i in line {
            if text_is_whitespace(&chars[i].text) {
                space_between = true;
                continue;
            }
            if let Some(a) = prev_solid {
                let gap = chars[i].left - chars[a].right;
                let h = chars[a].bottom - chars[a].top;
                if !space_between && gap > 0.12 * h && gap < 1.0 * h {
                    let prev = &chars[a];
                    let axis_len = (prev.transform[0] * prev.transform[0]
                        + prev.transform[1] * prev.transform[1])
                        .sqrt();
                    let transform = if axis_len > 1e-9 {
                        let ux = prev.transform[0] / axis_len;
                        let uy = prev.transform[1] / axis_len;
                        let end_projection = [
                            prev.left * ux + prev.top * uy,
                            prev.left * ux + prev.bottom * uy,
                            prev.right * ux + prev.top * uy,
                            prev.right * ux + prev.bottom * uy,
                        ]
                        .into_iter()
                        .fold(f64::NEG_INFINITY, f64::max);
                        let origin_projection = prev.transform[4] * ux + prev.transform[5] * uy;
                        translate_mat(
                            prev.transform,
                            (end_projection - origin_projection).max(0.0) / axis_len,
                            0.0,
                        )
                    } else {
                        prev.transform
                    };
                    let alen = (prev.advance[0] * prev.advance[0]
                        + prev.advance[1] * prev.advance[1])
                        .sqrt();
                    let advance = if alen > 1e-9 {
                        [prev.advance[0] / alen * gap, prev.advance[1] / alen * gap]
                    } else {
                        [gap, 0.0]
                    };
                    inserts.push((
                        a + 1,
                        PendingTextChar {
                            text: " ".into(),
                            left: prev.right,
                            right: chars[i].left,
                            top: prev.top,
                            bottom: prev.bottom,
                            transform,
                            advance,
                            glyph_width: None,
                            font: prev.font,
                            font_size: prev.font_size,
                            rot_q: prev.rot_q,
                            upright: prev.upright,
                            synthetic: true,
                        },
                    ));
                }
            }
            prev_solid = Some(i);
            space_between = false;
        }
    }

    if inserts.is_empty() {
        return;
    }
    // 挿入位置昇順で1回の merge（Vec::insert の逆順挿入と同一結果）
    inserts.sort_by(|a, b| a.0.cmp(&b.0));
    let old = std::mem::take(chars);
    let mut merged = Vec::with_capacity(old.len() + inserts.len());
    let mut it = inserts.into_iter().peekable();
    for (i, c) in old.into_iter().enumerate() {
        while let Some((_, sp)) = it.next_if(|(pos, _)| *pos == i) {
            merged.push(sp);
        }
        merged.push(c);
    }
    for (_, sp) in it {
        merged.push(sp);
    }
    *chars = merged;
}

fn text_is_whitespace(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_whitespace())
}

#[cfg(test)]
pub(crate) fn extract_edges(
    contents: &[u8],
    resources: &Dict,
    view: [f64; 4],
    ctx: ContentCtx<'_>,
) -> Vec<EdgePart> {
    extract_parts(contents, resources, view, ctx, 0).edges
}

fn identity() -> [f64; 6] {
    [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
}

/// m1 ∘ m2（m2 を先に適用）
fn mat_mul(m1: [f64; 6], m2: [f64; 6]) -> [f64; 6] {
    [
        m1[0] * m2[0] + m1[2] * m2[1],
        m1[1] * m2[0] + m1[3] * m2[1],
        m1[0] * m2[2] + m1[2] * m2[3],
        m1[1] * m2[2] + m1[3] * m2[3],
        m1[0] * m2[4] + m1[2] * m2[5] + m1[4],
        m1[1] * m2[4] + m1[3] * m2[5] + m1[5],
    ]
}

fn apply_ctm(m: [f64; 6], x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn translate_mat(m: [f64; 6], tx: f64, ty: f64) -> [f64; 6] {
    [
        m[0],
        m[1],
        m[2],
        m[3],
        m[0] * tx + m[2] * ty + m[4],
        m[1] * tx + m[3] * ty + m[5],
    ]
}

/// テキスト状態。q/Q で CTM と一緒に保存・復元
#[derive(Clone)]
struct TextState {
    text_matrix: [f64; 6],
    text_line_matrix: [f64; 6],
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    text_h_scale: f64,
    text_rise: f64,
    leading: f64,
    font: Option<Arc<LoadedFont>>,
    /// Tf で指定されたリソース名
    font_name: String,
}

impl TextState {
    fn new() -> Self {
        Self {
            text_matrix: identity(),
            text_line_matrix: identity(),
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            text_h_scale: 1.0,
            text_rise: 0.0,
            leading: 0.0,
            font: None,
            font_name: String::new(),
        }
    }

    /// textLineMatrix を (tx,ty) 平行移動して textMatrix に複製
    fn move_text(&mut self, tx: f64, ty: f64) {
        self.text_line_matrix = translate_mat(self.text_line_matrix, tx, ty);
        self.text_matrix = self.text_line_matrix;
    }

    /// 改行（0, -leading）
    fn next_line(&mut self) {
        self.move_text(0.0, -self.leading);
    }

    /// trm = ctm ∘ textMatrix ∘ tsm
    fn text_rendering_matrix(&self, ctm: [f64; 6]) -> [f64; 6] {
        let tsm = [
            self.font_size * self.text_h_scale,
            0.0,
            0.0,
            self.font_size,
            0.0,
            self.text_rise,
        ];
        mat_mul(ctm, mat_mul(self.text_matrix, tsm))
    }
}

/// パスセグメント
#[derive(Debug, Clone)]
enum Seg {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    /// 制御点は捨て、終点のみ保持
    CurveTo(f64, f64),
    Close,
}

struct PathBuilder {
    segs: Vec<Seg>,
    current: Option<(f64, f64)>,
}

impl PathBuilder {
    fn new() -> Self {
        Self {
            segs: Vec::new(),
            current: None,
        }
    }

    fn clear(&mut self) {
        self.segs.clear();
        self.current = None;
    }

    fn move_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::MoveTo(x, y));
        self.current = Some((x, y));
    }

    fn line_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::LineTo(x, y));
        self.current = Some((x, y));
    }

    fn curve_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::CurveTo(x, y));
        self.current = Some((x, y));
    }

    fn close(&mut self) {
        self.segs.push(Seg::Close);
        // 現在点の始点戻しは finalize 側
    }

    /// re の move+line×3+close への展開。退化時は1本線
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        if w == 0.0 || h == 0.0 {
            self.move_to(x, y);
            self.line_to(x + w, y + h);
            self.close();
        } else {
            self.move_to(x, y);
            self.line_to(x + w, y);
            self.line_to(x + w, y + h);
            self.line_to(x, y + h);
            self.close();
        }
    }
}

/// CTM 適用済み2点からの軸平行辺。位置は終点側
fn axis_edge(a: (f64, f64), b: (f64, f64), view: [f64; 4]) -> Option<EdgePart> {
    let (ox, oy) = (view[0], view[3]); // view 左上（PDF 座標）
    let (px, py) = a;
    let (x, y) = b;
    let dx = x - px;
    let dy = y - py;
    if dx.abs() <= EPS && dy.abs() > EPS {
        Some(EdgePart {
            orientation: Orientation::Vertical,
            left: x - ox,
            right: x - ox,
            top: oy - py.max(y),
            bottom: oy - py.min(y),
        })
    } else if dy.abs() <= EPS && dx.abs() > EPS {
        let yy = oy - y;
        Some(EdgePart {
            orientation: Orientation::Horizontal,
            left: px.min(x) - ox,
            right: px.max(x) - ox,
            top: yy,
            bottom: yy,
        })
    } else {
        None
    }
}

/// パスの辺化とバッファのクリア
///
/// 軸平行にならない線分と曲線はパス1回分の外接矩形として graphics へ集約
/// 弦長比率で軸平行辺が支配的なパスは罫線描画とみなし集約の対象外
fn finalize_path(
    path: &mut PathBuilder,
    filled: bool,
    ctm: [f64; 6],
    view: [f64; 4],
    edges: &mut Vec<EdgePart>,
    graphics: &mut Vec<GraphicPart>,
) {
    let segs = std::mem::take(&mut path.segs);
    path.current = None;

    let apply = |x: f64, y: f64| apply_ctm(ctm, x, y);
    let (ox, oy) = (view[0], view[3]);
    let mut gbox: Option<[f64; 4]> = None;
    let mut axis_len = 0.0;
    let mut gbox_len = 0.0;
    let mut emit = |a: (f64, f64), b: (f64, f64), curve: bool| {
        if !curve && let Some(e) = axis_edge(a, b, view) {
            axis_len += (a.0 - b.0).hypot(a.1 - b.1);
            edges.push(e);
            return;
        }
        if (a.0 - b.0).abs() <= 0.01 && (a.1 - b.1).abs() <= 0.01 {
            return;
        }
        gbox_len += (a.0 - b.0).hypot(a.1 - b.1);
        let (l, r) = (a.0.min(b.0) - ox, a.0.max(b.0) - ox);
        let (t, bo) = (oy - a.1.max(b.1), oy - a.1.min(b.1));
        gbox = Some(match gbox {
            Some(g) => [g[0].min(l), g[1].min(t), g[2].max(r), g[3].max(bo)],
            None => [l, t, r, bo],
        });
    };

    let mut sub_start: Option<(f64, f64)> = None;
    let mut prev: Option<(f64, f64)> = None;

    for seg in segs {
        match seg {
            Seg::MoveTo(x, y) => {
                let pt = apply(x, y);
                if filled {
                    if let (Some(s), Some(p)) = (sub_start, prev) {
                        emit(p, s, false);
                    }
                }
                sub_start = Some(pt);
                prev = Some(pt);
            }
            Seg::LineTo(x, y) => {
                let pt = apply(x, y);
                if let Some(p) = prev {
                    emit(p, pt, false);
                }
                prev = Some(pt);
            }
            Seg::CurveTo(x, y) => {
                let pt = apply(x, y);
                if let Some(p) = prev {
                    emit(p, pt, true);
                }
                prev = Some(pt);
            }
            Seg::Close => {
                if let (Some(s), Some(p)) = (sub_start, prev) {
                    emit(p, s, false);
                    prev = Some(s);
                }
            }
        }
    }

    if filled {
        if let (Some(s), Some(p)) = (sub_start, prev) {
            emit(p, s, false);
        }
    }
    if let Some(g) = gbox
        && gbox_len >= (axis_len + gbox_len) * GRAPHICS_MIN_FRAC
    {
        graphics.push(GraphicPart {
            left: g[0],
            top: g[1],
            right: g[2],
            bottom: g[3],
            curve_len: gbox_len,
        });
    }
}

/// 間接参照をキャッシュ共有のまま解決（読み取り用）
fn resolve_obj(obj: &Object, ctx: &ContentCtx<'_>) -> Arc<Object> {
    match obj {
        Object::Ref(r) => match ctx.xref {
            Some(xref) => xref
                .fetch_shared(*r, ctx.data)
                .unwrap_or_else(|_| Arc::new(Object::Null)),
            None => Arc::new(Object::Null),
        },
        other => Arc::new(other.clone()),
    }
}

struct FetchResolver<'a> {
    xref: Option<&'a XRef>,
    data: &'a [u8],
}

impl Resolver for FetchResolver<'_> {
    fn resolve(&self, r: Ref) -> crate::error::Result<Option<Object>> {
        // コンテンツ解釈は寛容。失敗は未解決扱い
        Ok(self.xref.and_then(|x| x.fetch(r, self.data).ok()))
    }
}

fn as_f64(obj: &Object) -> Option<f64> {
    match obj {
        Object::Int(n) => Some(*n as f64),
        Object::Real(n) => Some(*n),
        _ => None,
    }
}

/// スタック末尾 N 個の数値。順序保存、固定長でヒープ確保なし
fn take_nums<const N: usize>(stack: &mut Vec<Object>) -> Option<[f64; N]> {
    if stack.len() < N {
        return None;
    }
    let start = stack.len() - N;
    let mut out = [0.0; N];
    for (i, o) in stack[start..].iter().enumerate() {
        out[i] = as_f64(o)?;
    }
    stack.truncate(start);
    Some(out)
}

fn take_name(stack: &mut Vec<Object>) -> Option<String> {
    match stack.pop() {
        Some(Object::Name(n)) => Some(n.into_owned()),
        Some(other) => {
            stack.push(other);
            None
        }
        None => None,
    }
}

/// content stream 内の配列・辞書ネスト上限。stray closer は interpret の main loop で破棄
const MAX_STREAM_PARSE_DEPTH: usize = 200;

fn parse_array(lexer: &mut Lexer<'_>, depth: usize) -> Object {
    if depth >= MAX_STREAM_PARSE_DEPTH {
        return Object::Null;
    }
    let mut arr = Vec::new();
    loop {
        match lexer.next_token() {
            Token::ArrayEnd | Token::Eof => break,
            Token::ArrayStart => arr.push(parse_array(lexer, depth + 1)),
            Token::DictStart => arr.push(parse_dict(lexer, depth + 1)),
            Token::Int(n) => arr.push(Object::Int(n)),
            Token::Real(r) => arr.push(Object::Real(r)),
            Token::Str(s) => arr.push(Object::Str(s)),
            Token::Name(n) => arr.push(Object::Name(n)),
            Token::Cmd(s) => match s {
                Cmd::Op(Op::True) => arr.push(Object::Bool(true)),
                Cmd::Op(Op::False) => arr.push(Object::Bool(false)),
                Cmd::Op(Op::Null) => arr.push(Object::Null),
                Cmd::Op(Op::R) => {
                    // n g R
                    if arr.len() >= 2 {
                        let g = arr.pop();
                        let n = arr.pop();
                        if let (Some(Object::Int(num)), Some(Object::Int(generation))) = (n, g) {
                            if (0..=i64::from(u32::MAX)).contains(&num)
                                && (0..=i64::from(u16::MAX)).contains(&generation)
                            {
                                arr.push(Object::Ref(Ref::new(num as u32, generation as u16)));
                                continue;
                            }
                        }
                        // 復元不能は破棄
                    }
                }
                _ => {} // 配列内の未知コマンドは無視
            },
            Token::DictEnd | Token::BraceStart | Token::BraceEnd => {}
        }
    }
    Object::Array(arr)
}

fn parse_dict(lexer: &mut Lexer<'_>, depth: usize) -> Object {
    if depth >= MAX_STREAM_PARSE_DEPTH {
        return Object::Null;
    }
    let mut dict = Dict::new();
    loop {
        match lexer.next_token() {
            Token::DictEnd | Token::Eof => break,
            Token::Name(key) => {
                let value = match lexer.next_token() {
                    Token::ArrayStart => parse_array(lexer, depth + 1),
                    Token::DictStart => parse_dict(lexer, depth + 1),
                    Token::Int(n) => Object::Int(n),
                    Token::Real(r) => Object::Real(r),
                    Token::Str(s) => Object::Str(s),
                    Token::Name(n) => Object::Name(n),
                    Token::Cmd(s) => match s {
                        Cmd::Op(Op::True) => Object::Bool(true),
                        Cmd::Op(Op::False) => Object::Bool(false),
                        Cmd::Op(Op::Null) => Object::Null,
                        _ => Object::Null,
                    },
                    Token::Eof | Token::DictEnd | Token::ArrayEnd => break,
                    _ => Object::Null,
                };
                dict.set(key, value);
            }
            Token::ArrayStart => {
                let _ = parse_array(lexer, depth + 1);
            }
            Token::DictStart => {
                let _ = parse_dict(lexer, depth + 1);
            }
            _ => {}
        }
    }
    Object::Dict(dict)
}

/// BI...EI のスキップと lexer 位置の更新
fn skip_inline_image_in(data: &[u8], lexer: &mut Lexer<'_>) {
    loop {
        match lexer.next_token() {
            Token::Cmd(Cmd::Op(Op::ID)) => break,
            Token::Eof => return,
            _ => {}
        }
    }

    let mut pos = lexer.byte_pos();
    // ID 直後の空白を1回分スキップ（改行含む）
    while pos < data.len() && is_pdf_ws(data[pos]) {
        pos += 1;
    }

    // 探索対象は「空白に続く EI、直後が空白または終端」
    let mut i = pos;
    while i + 2 <= data.len() {
        if data[i] == b'E' && data[i + 1] == b'I' {
            let prev_ws = i == 0 || is_pdf_ws(data[i - 1]);
            // 画像データ先頭の EI も仕様の「空白に続く」を満たす扱い
            // ID 後の空白はスキップ済みのため i == pos は true
            let prev_ok = if i == pos {
                // ID 後空白の直後の EI は空画像として許容
                true
            } else {
                prev_ws
            };
            let next_ok = i + 2 >= data.len() || is_pdf_ws(data[i + 2]);
            if prev_ok && next_ok {
                // EI の直後へ
                let after = i + 2;
                lexer.set_pos(after);
                return;
            }
        }
        i += 1;
    }
    // 見つからなければ末尾へ
    lexer.set_pos(data.len());
}

fn is_pdf_ws(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0a | 0x0c | 0x0d | 0x20)
}

const IDENTITY_MAT: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// コンテンツストリーム評価器。ページ内共有の出力先と文脈
struct Evaluator<'a, 'b> {
    ctx: &'a ContentCtx<'b>,
    view: [f64; 4],
    glyphs: &'a mut Vec<(GlyphPart, RotQ)>,
    edges: &'a mut Vec<EdgePart>,
    graphics: &'a mut Vec<GraphicPart>,
    visiting: &'a mut HashSet<Ref>,
    /// Form 再帰の現在深さ 間接参照 inline 双方
    form_depth: usize,
    text_sink: Option<&'a mut TextSink>,
    /// edges/graphics を集めるか
    collect_paths: bool,
    /// glyphs を集めるか
    collect_glyphs: bool,
}

/// interpret 1回分（1ストリーム分）の可変状態
struct Frame<'r> {
    stack: Vec<Object>,
    ctm: [f64; 6],
    gstack: Vec<([f64; 6], TextState)>,
    /// gstack 上限超過で打ち切った q の数。対の Q が先に消費
    gstack_overflow: usize,
    path: PathBuilder,
    text: TextState,
    /// この resources スコープ内の Tf 名前 → フォント
    font_memo: HashMap<String, Arc<LoadedFont>>,
    resources: &'r Dict,
    /// パス構築・ペイントを edges/graphics に反映するか
    collect_paths: bool,
    /// TJ 配列展開の使い回しバッファ
    tj_scratch: Vec<u8>,
}

impl Evaluator<'_, '_> {
    /// ペイント系 op の共通処理（`collect_paths` オフではパスを捨てる）
    fn paint_or_drop(
        &mut self,
        path: &mut PathBuilder,
        ctm: [f64; 6],
        fill: bool,
        close: bool,
        collect_paths: bool,
    ) {
        if !collect_paths {
            path.clear();
            return;
        }
        if close {
            path.close();
        }
        finalize_path(path, fill, ctm, self.view, self.edges, self.graphics);
    }

    fn interpret(&mut self, contents: &[u8], resources: &Dict, ctm0: [f64; 6]) {
        let mut lexer = Lexer::new(contents);
        let mut frame = Frame {
            stack: Vec::new(),
            ctm: ctm0,
            gstack: Vec::new(),
            gstack_overflow: 0,
            path: PathBuilder::new(),
            text: TextState::new(),
            font_memo: HashMap::new(),
            resources,
            collect_paths: self.collect_paths,
            tj_scratch: Vec::new(),
        };

        loop {
            let token = lexer.next_token();
            let stack = &mut frame.stack;
            match token {
                Token::Eof => break,
                Token::Int(n) => stack.push(Object::Int(n)),
                Token::Real(r) => stack.push(Object::Real(r)),
                Token::Str(s) => stack.push(Object::Str(s)),
                Token::Name(n) => stack.push(Object::Name(n)),
                Token::ArrayStart => stack.push(parse_array(&mut lexer, 0)),
                Token::DictStart => stack.push(parse_dict(&mut lexer, 0)),
                Token::ArrayEnd | Token::DictEnd | Token::BraceStart | Token::BraceEnd => {
                    // 単独の区切りは無視
                }
                Token::Cmd(Cmd::Op(Op::True)) => stack.push(Object::Bool(true)),
                Token::Cmd(Cmd::Op(Op::False)) => stack.push(Object::Bool(false)),
                Token::Cmd(Cmd::Op(Op::Null)) => stack.push(Object::Null),
                Token::Cmd(Cmd::Op(Op::R)) => {
                    // n g R。i64 のまま扱い f64 経由の精度損失を避ける（配列パーサと同じ規則）
                    if stack.len() >= 2 {
                        let generation = stack.pop();
                        let num = stack.pop();
                        if let (Some(Object::Int(num)), Some(Object::Int(generation))) =
                            (num, generation)
                            && (0..=i64::from(u32::MAX)).contains(&num)
                            && (0..=i64::from(u16::MAX)).contains(&generation)
                        {
                            stack.push(Object::Ref(Ref::new(num as u32, generation as u16)));
                        }
                        // 復元不能は破棄
                    }
                }
                Token::Cmd(Cmd::Op(Op::BI)) => {
                    stack.clear();
                    skip_inline_image_in(contents, &mut lexer);
                }
                Token::Cmd(Cmd::Op(op)) => {
                    self.dispatch_op(op, &mut frame);
                    frame.stack.clear();
                }
                Token::Cmd(Cmd::Other(_)) => {
                    frame.stack.clear();
                }
            }
        }
    }

    fn dispatch_op(&mut self, op: Op, frame: &mut Frame<'_>) {
        let Frame {
            stack,
            ctm,
            gstack,
            gstack_overflow,
            path,
            text,
            font_memo,
            resources,
            collect_paths,
            tj_scratch,
        } = frame;
        match op {
            // グラフィックス状態。テキスト状態も一緒に保存・復元、アンダーフローは無視
            // 上限超過の q はスキップカウンタで数え対の Q で消費
            Op::LowerQ => {
                if gstack.len() < MAX_GSTACK_DEPTH {
                    gstack.push((*ctm, text.clone()));
                } else {
                    *gstack_overflow += 1;
                }
            }
            Op::Q => {
                if *gstack_overflow > 0 {
                    *gstack_overflow -= 1;
                } else if let Some((m, t)) = gstack.pop() {
                    *ctm = m;
                    *text = t;
                }
            }
            Op::Cm => {
                if let Some(n) = take_nums::<6>(stack) {
                    let m = [n[0], n[1], n[2], n[3], n[4], n[5]];
                    *ctm = mat_mul(*ctm, m);
                }
            }

            // パス構築（text_sink 経路では edges/graphics を捨てるため構築しない）
            Op::LowerM => {
                if *collect_paths
                    && let Some(n) = take_nums::<2>(stack)
                {
                    path.move_to(n[0], n[1]);
                }
            }
            Op::LowerL => {
                if *collect_paths
                    && let Some(n) = take_nums::<2>(stack)
                {
                    path.line_to(n[0], n[1]);
                }
            }
            Op::LowerC => {
                if *collect_paths
                    && let Some(n) = take_nums::<6>(stack)
                {
                    path.curve_to(n[4], n[5]);
                }
            }
            Op::LowerV | Op::LowerY => {
                if *collect_paths
                    && let Some(n) = take_nums::<4>(stack)
                {
                    path.curve_to(n[2], n[3]);
                }
            }
            Op::LowerH => {
                if *collect_paths {
                    path.close();
                }
            }
            Op::Re => {
                if *collect_paths
                    && let Some(n) = take_nums::<4>(stack)
                {
                    path.rect(n[0], n[1], n[2], n[3]);
                }
            }

            // ペイント（close 系は確定前に閉じを追加。text_sink 経路は捨てる）
            Op::S => self.paint_or_drop(path, *ctm, false, false, *collect_paths),
            Op::LowerS => self.paint_or_drop(path, *ctm, false, true, *collect_paths),
            Op::LowerF | Op::F | Op::LowerFStar | Op::B | Op::BStar => {
                self.paint_or_drop(path, *ctm, true, false, *collect_paths)
            }
            Op::LowerB | Op::LowerBStar => {
                self.paint_or_drop(path, *ctm, true, true, *collect_paths)
            }
            Op::LowerN => path.clear(),

            // クリップ・線属性は無視
            Op::W | Op::WStar | Op::LowerW | Op::LowerD | Op::J | Op::LowerJ | Op::M => {}

            // Form XObject
            Op::Do => {
                if let Some(name) = take_name(stack) {
                    self.do_form_xobject(&name, *ctm, resources);
                }
            }

            // テキスト状態
            Op::BT => {
                text.text_matrix = identity();
                text.text_line_matrix = identity();
            }
            Op::ET => {}
            Op::Tf => {
                // /Name size Tf → stack [Name, size]
                if let Some(sz) = take_nums::<1>(stack) {
                    if let Some(name) = take_name(stack) {
                        text.font_size = sz[0];
                        // 上限到達後は insert なし。未知名は都度 lookup_font で解決
                        let font = match font_memo.get(&name) {
                            Some(f) => Arc::clone(f),
                            None => {
                                let f = lookup_font(resources, &name, self.ctx.data, self.ctx.xref);
                                if font_memo.len() < MAX_FONT_MEMO_ENTRIES {
                                    font_memo.insert(name.clone(), Arc::clone(&f));
                                }
                                f
                            }
                        };
                        text.font = Some(font);
                        text.font_name = name;
                    }
                }
            }
            Op::Tc => {
                if let Some(n) = take_nums::<1>(stack) {
                    text.char_spacing = n[0];
                }
            }
            Op::Tw => {
                if let Some(n) = take_nums::<1>(stack) {
                    text.word_spacing = n[0];
                }
            }
            Op::Tz => {
                if let Some(n) = take_nums::<1>(stack) {
                    text.text_h_scale = n[0] / 100.0;
                }
            }
            Op::TL => {
                if let Some(n) = take_nums::<1>(stack) {
                    text.leading = n[0];
                }
            }
            Op::Ts => {
                if let Some(n) = take_nums::<1>(stack) {
                    text.text_rise = n[0];
                }
            }
            Op::Tr => {
                // レンダリングモードは無視（不可視も出力）
                let _ = take_nums::<1>(stack);
            }
            Op::Td => {
                if let Some(n) = take_nums::<2>(stack) {
                    text.move_text(n[0], n[1]);
                }
            }
            Op::TD => {
                if let Some(n) = take_nums::<2>(stack) {
                    text.leading = -n[1];
                    text.move_text(n[0], n[1]);
                }
            }
            Op::Tm => {
                if let Some(n) = take_nums::<6>(stack) {
                    let m = [n[0], n[1], n[2], n[3], n[4], n[5]];
                    text.text_matrix = m;
                    text.text_line_matrix = m;
                }
            }
            Op::TStar => text.next_line(),
            Op::Tj => {
                if let Some(Object::Str(s)) = stack.pop() {
                    let font = text.font.clone();
                    self.show_text_bytes(text, font.as_ref(), &s, 0.0, *ctm);
                }
            }
            Op::TJ => {
                if let Some(Object::Array(arr)) = stack.pop() {
                    let font = text.font.clone();
                    self.show_text_array(text, font.as_ref(), &arr, *ctm, tj_scratch);
                }
            }
            Op::Quote => {
                // 改行してから Tj
                if let Some(Object::Str(s)) = stack.pop() {
                    text.next_line();
                    let font = text.font.clone();
                    self.show_text_bytes(text, font.as_ref(), &s, 0.0, *ctm);
                }
            }
            Op::DblQuote => {
                // aw ac string " → Tw Tc T* Tj
                if let Some(Object::Str(s)) = stack.pop() {
                    if let Some(n) = take_nums::<2>(stack) {
                        text.word_spacing = n[0];
                        text.char_spacing = n[1];
                        text.next_line();
                        let font = text.font.clone();
                        self.show_text_bytes(text, font.as_ref(), &s, 0.0, *ctm);
                    }
                }
            }

            // その他は無視
            _ => {}
        }
    }

    fn show_text_array(
        &mut self,
        text: &mut TextState,
        font: Option<&Arc<LoadedFont>>,
        arr: &[Object],
        ctm: [f64; 6],
        buf: &mut Vec<u8>,
    ) {
        buf.clear();
        for item in arr {
            match item {
                Object::Str(s) => buf.extend_from_slice(s),
                Object::Int(n) => {
                    let n = *n as f64;
                    if n != 0.0 {
                        let extra = -n / 1000.0 * text.font_size;
                        self.show_text_bytes(text, font, buf, extra, ctm);
                        buf.clear();
                    }
                }
                Object::Real(n) => {
                    if *n != 0.0 {
                        let extra = -n / 1000.0 * text.font_size;
                        self.show_text_bytes(text, font, buf, extra, ctm);
                        buf.clear();
                    }
                }
                _ => {}
            }
        }
        if !buf.is_empty() {
            self.show_text_bytes(text, font, buf, 0.0, ctm);
        }
    }

    /// extra_spacing は最終グリフの字送りへの加算分（テキスト空間、hScale 前）
    /// 横書きは -n/1000×fs と同符号
    fn show_text_bytes(
        &mut self,
        text: &mut TextState,
        font: Option<&Arc<LoadedFont>>,
        bytes: &[u8],
        extra_spacing: f64,
        ctm: [f64; 6],
    ) {
        let Some(font_arc) = font else {
            // フォント未設定時は extra だけの平行移動（TJ 数値のみ）
            if extra_spacing != 0.0 {
                let dx = extra_spacing * text.text_h_scale;
                text.text_matrix = translate_mat(text.text_matrix, dx, 0.0);
            }
            return;
        };
        let font = &**font_arc;
        if font.vertical() {
            self.show_text_bytes_vertical(text, font, bytes, extra_spacing, ctm);
            return;
        }
        let view = self.view;
        let collect_glyphs = self.collect_glyphs;
        let glyphs = &mut *self.glyphs;
        let mut text_sink = self.text_sink.as_deref_mut();

        if bytes.is_empty() {
            if extra_spacing != 0.0 {
                let dx = (text.char_spacing + extra_spacing) * text.text_h_scale;
                text.text_matrix = translate_mat(text.text_matrix, dx, 0.0);
            }
            return;
        }

        let count = font.glyph_count(bytes);
        let (view_left, view_top) = (view[0], view[3]);
        let ascent = font.ascent();
        let descent = font.descent();
        let font_idx = text_sink
            .as_mut()
            .map(|s| s.font_index(font, &text.font_name));

        // 列内で不変: TRM 線形成分と派生量（平行移動のみ変わる）
        // text_rendering_matrix と同じ式・同じ演算順の一括算出
        let trm_lin = text.text_rendering_matrix(ctm);
        let font_size_dev = (trm_lin[2] * trm_lin[2] + trm_lin[3] * trm_lin[3]).sqrt();
        let dlen = (trm_lin[0] * trm_lin[0] + trm_lin[1] * trm_lin[1]).sqrt();
        let (ux, uy) = if dlen > 0.0 {
            (trm_lin[0] / dlen, trm_lin[1] / dlen)
        } else {
            (1.0, 0.0)
        };
        let upright = uy.abs() <= 0.05 && ux > 0.05;
        let rot = quantize_rot(ux, uy);
        let a_off = ascent * font_size_dev;
        let d_off = descent * font_size_dev;
        // end_trm 用 tsm（text_rendering_matrix 内と同じ要素順）
        let tsm = [
            text.font_size * text.text_h_scale,
            0.0,
            0.0,
            text.font_size,
            0.0,
            text.text_rise,
        ];

        let mut i = 0;
        font.for_each_glyph(bytes, |g| {
            let unicode = g.unicode;
            let extra = if i + 1 == count { extra_spacing } else { 0.0 };
            i += 1;

            // 平行移動成分はグリフごとに再計算。線形成分は trm_lin
            let trm = text.text_rendering_matrix(ctm);
            let (ox, oy) = (trm[4], trm[5]);

            // scaledDim = 幅/1000 × fontSize × hScale
            let scaled_dim = g.width / 1000.0 * text.font_size * text.text_h_scale;
            let mut spacing = text.char_spacing;
            if g.is_space {
                spacing += text.word_spacing;
            }
            // bbox 用（Tc/Tw は含む、TJ 数値 extra は位置のみ）
            let bbox_advance = scaled_dim + spacing * text.text_h_scale;
            // textMatrix の前進（TJ extra を加算）
            let advance = bbox_advance + extra * text.text_h_scale;

            // デバイス空間での実効字送り幅（bbox 用。進行方向への符号付き長）
            let end_tm = translate_mat(text.text_matrix, bbox_advance, 0.0);
            let end_trm = mat_mul(ctm, mat_mul(end_tm, tsm));
            let (ex, ey) = (end_trm[4], end_trm[5]);
            let device_w = (ex - ox) * ux + (ey - oy) * uy;

            // 表経路: コードポイントごとに分割し幅を等分
            if collect_glyphs {
                let nch = unicode.chars().count().max(1);
                let per_w = device_w / nch as f64;
                for (k, ch) in unicode.chars().enumerate() {
                    if ch == '\n' || ch == '\r' || ch == '\t' {
                        continue;
                    }
                    let (x0, x1, y0, y1) = glyph_box(
                        ox,
                        oy,
                        ux,
                        uy,
                        per_w * k as f64,
                        per_w * (k as f64 + 1.0),
                        a_off,
                        d_off,
                    );
                    glyphs.push((
                        GlyphPart {
                            ch,
                            left: x0 - view_left,
                            right: x1 - view_left,
                            top: view_top - y1,
                            bottom: view_top - y0,
                            font_size: Some(font_size_dev),
                            upright,
                            rot: 0,
                        },
                        rot,
                    ));
                }
            }

            if let (Some(sink), Some(fi)) = (text_sink.as_mut(), font_idx) {
                let text_str: String = unicode
                    .chars()
                    .filter(|&ch| ch != '\n' && ch != '\r' && ch != '\t')
                    .collect();
                if !text_str.is_empty() {
                    let (x0, x1, y0, y1) = glyph_box(ox, oy, ux, uy, 0.0, device_w, a_off, d_off);
                    // 公称字送り（Tc・Tw・TJ なし）のデバイス長
                    let pure_end_tm = translate_mat(text.text_matrix, scaled_dim, 0.0);
                    let pure_end_trm = mat_mul(ctm, mat_mul(pure_end_tm, tsm));
                    let pure_device_w =
                        (pure_end_trm[4] - ox) * ux + (pure_end_trm[5] - oy) * uy;
                    // 前進ベクトル（TJ 含む、表示座標）
                    let end_tm_full = translate_mat(text.text_matrix, advance, 0.0);
                    let end_trm_full = mat_mul(ctm, mat_mul(end_tm_full, tsm));
                    let (fx, fy) = (end_trm_full[4], end_trm_full[5]);
                    sink.chars.push(PendingTextChar {
                        text: text_str,
                        left: x0 - view_left,
                        right: x1 - view_left,
                        top: view_top - y1,
                        bottom: view_top - y0,
                        transform: [
                            trm_lin[0],
                            -trm_lin[1],
                            trm_lin[2],
                            -trm_lin[3],
                            trm[4] - view_left,
                            view_top - trm[5],
                        ],
                        advance: [fx - ox, -(fy - oy)],
                        glyph_width: Some(pure_device_w.max(0.0)),
                        font: fi,
                        font_size: font_size_dev,
                        rot_q: rot,
                        upright,
                        synthetic: false,
                    });
                }
            }

            text.text_matrix = translate_mat(text.text_matrix, advance, 0.0);
        });
    }

    fn show_text_bytes_vertical(
        &mut self,
        text: &mut TextState,
        font: &LoadedFont,
        bytes: &[u8],
        extra_spacing: f64,
        ctm: [f64; 6],
    ) {
        let view = self.view;
        let collect_glyphs = self.collect_glyphs;
        let glyphs = &mut *self.glyphs;
        let mut text_sink = self.text_sink.as_deref_mut();

        // TJ の extra は横書き向け -n/1000×fs。縦書きは符号反転
        let vert_extra = |e: f64| -e;

        if bytes.is_empty() {
            if extra_spacing != 0.0 {
                let cs = -text.char_spacing + vert_extra(extra_spacing);
                text.text_matrix = translate_mat(text.text_matrix, 0.0, -cs);
            }
            return;
        }

        let count = font.glyph_count(bytes);
        let (view_left, view_top) = (view[0], view[3]);
        let ascent = font.ascent();
        let descent = font.descent();
        let font_idx = text_sink
            .as_mut()
            .map(|s| s.font_index(font, &text.font_name));

        // 列内で不変: TRM 線形成分と派生量（平行移動のみ変わる）
        let trm_lin = text.text_rendering_matrix(ctm);
        let font_size_dev = (trm_lin[2] * trm_lin[2] + trm_lin[3] * trm_lin[3]).sqrt();
        let dlen = (trm_lin[0] * trm_lin[0] + trm_lin[1] * trm_lin[1]).sqrt();
        let (ux, uy) = if dlen > 0.0 {
            (trm_lin[0] / dlen, trm_lin[1] / dlen)
        } else {
            (1.0, 0.0)
        };
        let upright = uy.abs() <= 0.05 && ux > 0.05;
        let rot = quantize_rot(ux, uy);
        let a_off = ascent * font_size_dev;
        let d_off = descent * font_size_dev;
        let tsm = [
            text.font_size * text.text_h_scale,
            0.0,
            0.0,
            text.font_size,
            0.0,
            text.text_rise,
        ];

        let mut i = 0;
        font.for_each_glyph(bytes, |g| {
            let unicode = g.unicode;
            let width = g.width;
            let extra = if i + 1 == count {
                vert_extra(extra_spacing)
            } else {
                0.0
            };
            i += 1;
            // [w1y, v1x, v1y]。欠落時は横幅の負値と半角オフセット
            let [w1y, v1x, v1y] = g.vmetric.unwrap_or([-width, width * 0.5, 880.0]);

            let trm = text.text_rendering_matrix(ctm);
            // テキスト状態の原点（v1 ずらし前）。transform / advance の基準
            let (wx, wy) = (trm[4], trm[5]);
            // 描画原点: canvas の vx=-v1x, vy=v1y（線形成分は trm_lin）
            let ox = wx + trm_lin[0] * (-v1x / 1000.0) + trm_lin[2] * (v1y / 1000.0);
            let oy = wy + trm_lin[1] * (-v1x / 1000.0) + trm_lin[3] * (v1y / 1000.0);

            // 縦前進（hScale なし）。Tc は符号反転
            let scaled_dim = w1y / 1000.0 * text.font_size;
            let mut cs = -text.char_spacing + extra;
            if g.is_code_space {
                cs += text.word_spacing;
            }
            let advance_y = scaled_dim - cs;

            // TRM 上の幅端点の横軸射影。軸の鏡映は ux/uy のみ
            let width_end_x = ox + trm_lin[0] * (width / 1000.0);
            let width_end_y = oy + trm_lin[1] * (width / 1000.0);
            let device_w = (width_end_x - ox) * ux + (width_end_y - oy) * uy;

            if collect_glyphs {
                let nch = unicode.chars().count().max(1);
                let per_w = device_w / nch as f64;
                for (k, ch) in unicode.chars().enumerate() {
                    if ch == '\n' || ch == '\r' || ch == '\t' {
                        continue;
                    }
                    let (x0, x1, y0, y1) = glyph_box(
                        ox,
                        oy,
                        ux,
                        uy,
                        per_w * k as f64,
                        per_w * (k as f64 + 1.0),
                        a_off,
                        d_off,
                    );
                    glyphs.push((
                        GlyphPart {
                            ch,
                            left: x0 - view_left,
                            right: x1 - view_left,
                            top: view_top - y1,
                            bottom: view_top - y0,
                            font_size: Some(font_size_dev),
                            upright,
                            rot: 0,
                        },
                        rot,
                    ));
                }
            }

            if let (Some(sink), Some(fi)) = (text_sink.as_mut(), font_idx) {
                let text_str: String = unicode
                    .chars()
                    .filter(|&ch| ch != '\n' && ch != '\r' && ch != '\t')
                    .collect();
                if !text_str.is_empty() {
                    let (x0, x1, y0, y1) = glyph_box(ox, oy, ux, uy, 0.0, device_w, a_off, d_off);
                    let end_tm_full = translate_mat(text.text_matrix, 0.0, advance_y);
                    let end_trm_full = mat_mul(ctm, mat_mul(end_tm_full, tsm));
                    let (fx, fy) = (end_trm_full[4], end_trm_full[5]);
                    sink.chars.push(PendingTextChar {
                        text: text_str,
                        left: x0 - view_left,
                        right: x1 - view_left,
                        top: view_top - y1,
                        bottom: view_top - y0,
                        transform: [
                            trm_lin[0],
                            -trm_lin[1],
                            trm_lin[2],
                            -trm_lin[3],
                            wx - view_left,
                            view_top - wy,
                        ],
                        advance: [fx - wx, -(fy - wy)],
                        glyph_width: None,
                        font: fi,
                        font_size: font_size_dev,
                        rot_q: rot,
                        upright,
                        synthetic: false,
                    });
                }
            }

            text.text_matrix = translate_mat(text.text_matrix, 0.0, advance_y);
        });
    }
}

/// 字箱の外接軸平行矩形
///
/// 0/90/180/270度では正確な箱 任意角では外接箱
/// 返り値はPDF空間の (x_min, x_max, y_min, y_max)
fn glyph_box(
    ox: f64,
    oy: f64,
    ux: f64,
    uy: f64,
    s: f64,
    t: f64,
    a_off: f64,
    d_off: f64,
) -> (f64, f64, f64, f64) {
    // 上方向は進行方向を+90度回した向き
    let (px, py) = (-uy, ux);
    let xs = [
        ox + ux * s + px * d_off,
        ox + ux * s + px * a_off,
        ox + ux * t + px * d_off,
        ox + ux * t + px * a_off,
    ];
    let ys = [
        oy + uy * s + py * d_off,
        oy + uy * s + py * a_off,
        oy + uy * t + py * d_off,
        oy + uy * t + py * a_off,
    ];
    let fold = |v: &[f64; 4]| {
        v.iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &x| {
                (lo.min(x), hi.max(x))
            })
    };
    let (x0, x1) = fold(&xs);
    let (y0, y1) = fold(&ys);
    (x0, x1, y0, y1)
}

impl Evaluator<'_, '_> {
    fn do_form_xobject(&mut self, name: &str, ctm: [f64; 6], resources: &Dict) {
        // 上限以上のネストは再帰なし。間接参照 inline 双方
        if self.form_depth >= MAX_FORM_DEPTH {
            return;
        }

        let Some(xo_obj) = resources.get("XObject") else {
            return;
        };
        let xo = resolve_obj(xo_obj, self.ctx);
        let Object::Dict(xo_dict) = &*xo else {
            return;
        };
        let Some(entry) = xo_dict.get(name) else {
            return;
        };

        // 循環検出（参照番号）。挿入と除去はここで対
        let ref_id = match entry {
            Object::Ref(r) => Some(*r),
            _ => None,
        };
        if let Some(r) = ref_id
            && !self.visiting.insert(r)
        {
            return;
        }

        self.form_depth += 1;
        self.run_form_xobject(entry, ref_id, ctm, resources);
        self.form_depth -= 1;

        if let Some(r) = ref_id {
            self.visiting.remove(&r);
        }
    }

    /// Form XObject の解決と本文実行。循環検出の出入りは呼び出し側
    fn run_form_xobject(
        &mut self,
        entry: &Object,
        ref_id: Option<Ref>,
        ctm: [f64; 6],
        resources: &Dict,
    ) {
        let resolved = resolve_obj(entry, self.ctx);
        let Object::Stream(stream) = &*resolved else {
            return;
        };

        // Subtype = Form のみ
        let subtype = stream.dict.get("Subtype").map(|o| resolve_obj(o, self.ctx));
        if !matches!(subtype.as_deref(), Some(Object::Name(n)) if n == "Form") {
            return;
        }

        let form_mat = parse_matrix(stream.dict.get("Matrix"), self.ctx).unwrap_or_else(identity);

        let resolved_res = stream
            .dict
            .get("Resources")
            .map(|o| resolve_obj(o, self.ctx));
        let form_res: &Dict = match resolved_res.as_deref() {
            Some(Object::Dict(d)) => d,
            _ => resources,
        };

        // 本文デコード（間接参照は文書単位でメモ化。解釈自体は毎回）
        let Some(body) = form_stream_body(ref_id, stream, self.ctx) else {
            return;
        };

        // テキスト状態は Form 内で独立
        self.interpret(body.as_ref(), form_res, mat_mul(ctm, form_mat));
    }
}

/// Form 本文の取得。間接参照は文書単位キャッシュ
fn form_stream_body(
    ref_id: Option<Ref>,
    stream: &Stream,
    ctx: &ContentCtx<'_>,
) -> Option<Arc<Vec<u8>>> {
    if let (Some(r), Some(xref)) = (ref_id, ctx.xref) {
        if let Ok(guard) = xref.form_body_cache.lock() {
            if let Some(cached) = guard.get(&r.num) {
                return Some(Arc::clone(cached));
            }
        }
    }

    let resolver = FetchResolver {
        xref: ctx.xref,
        data: ctx.data,
    };
    let decoded = match decode_stream(
        ctx.data,
        stream,
        &resolver,
        ctx.xref.and_then(|x| x.cipher()),
        ctx.xref.map_or(crate::extract::DEFAULT_MAX_DECODED_BYTES, |x| {
            x.decode_limit()
        }),
    ) {
        Ok(b) => b,
        // インライン Stream は data が本文そのものの場合あり
        Err(_) => {
            let end = stream.start.saturating_add(stream.length);
            if end <= ctx.data.len() {
                ctx.data[stream.start..end].to_vec()
            } else {
                return None;
            }
        }
    };
    let body = Arc::new(decoded);

    if let (Some(r), Some(xref)) = (ref_id, ctx.xref) {
        if let Ok(mut guard) = xref.form_body_cache.lock() {
            if let Some(cached) = guard.get(&r.num) {
                return Some(Arc::clone(cached));
            }
            guard.insert(r.num, Arc::clone(&body));
        }
    }
    Some(body)
}

fn parse_matrix(obj: Option<&Object>, ctx: &ContentCtx<'_>) -> Option<[f64; 6]> {
    let obj = resolve_obj(obj?, ctx);
    let Object::Array(arr) = &*obj else {
        return None;
    };
    if arr.len() < 6 {
        return None;
    }
    Some([
        as_f64(&arr[0])?,
        as_f64(&arr[1])?,
        as_f64(&arr[2])?,
        as_f64(&arr[3])?,
        as_f64(&arr[4])?,
        as_f64(&arr[5])?,
    ])
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::object::Stream;

    fn edges_of(contents: &[u8], view: [f64; 4]) -> Vec<EdgePart> {
        extract_edges(
            contents,
            &Dict::new(),
            view,
            ContentCtx {
                data: &[],
                xref: None,
            },
        )
    }

    fn parts_of(contents: &[u8], view: [f64; 4]) -> ExtractedParts {
        extract_parts(
            contents,
            &Dict::new(),
            view,
            ContentCtx {
                data: &[],
                xref: None,
            },
            0,
        )
    }

    fn default_view() -> [f64; 4] {
        [0.0, 0.0, 200.0, 200.0]
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn find_edge<'a>(
        edges: &'a [EdgePart],
        orient: Orientation,
        left: f64,
        right: f64,
        top: f64,
        bottom: f64,
    ) -> bool {
        edges.iter().any(|e| {
            e.orientation == orient
                && approx(e.left, left)
                && approx(e.right, right)
                && approx(e.top, top)
                && approx(e.bottom, bottom)
        })
    }

    /// 90度正規化の座標移送
    #[test]
    fn rotate_box_td_90_moves_axes() {
        let rect = Rect {
            left: 10.0,
            top: 20.0,
            right: 30.0,
            bottom: 40.0,
        }
        .rotate_td(100.0, 200.0, 1);
        assert!(
            approx(rect.left, 160.0)
                && approx(rect.top, 10.0)
                && approx(rect.right, 180.0)
                && approx(rect.bottom, 30.0)
        );
    }

    /// 270度正規化の座標移送
    #[test]
    fn rotate_box_td_270_moves_axes() {
        let rect = Rect {
            left: 10.0,
            top: 20.0,
            right: 30.0,
            bottom: 40.0,
        }
        .rotate_td(100.0, 200.0, 3);
        assert!(
            approx(rect.left, 20.0)
                && approx(rect.top, 70.0)
                && approx(rect.right, 40.0)
                && approx(rect.bottom, 90.0)
        );
    }

    /// 正立: xが進行区間 yがディセント〜アセント
    #[test]
    fn glyph_box_upright_matches_axes() {
        let (x0, x1, y0, y1) = glyph_box(10.0, 100.0, 1.0, 0.0, 0.0, 5.0, 8.0, -2.0);
        assert!(approx(x0, 10.0) && approx(x1, 15.0));
        assert!(approx(y0, 98.0) && approx(y1, 108.0));
    }

    /// 90度回転: 幅がアセント幅になり 進行区間がyへ移る
    #[test]
    fn glyph_box_rotated_90_has_width() {
        let (x0, x1, y0, y1) = glyph_box(10.0, 100.0, 0.0, 1.0, 0.0, 5.0, 8.0, -2.0);
        assert!(approx(x0, 2.0) && approx(x1, 12.0));
        assert!(approx(y0, 100.0) && approx(y1, 105.0));
    }

    /// /Rotate 90 で線分が表示座標系へ回る
    #[test]
    fn page_rotate_moves_edges_to_display_frame() {
        let ExtractedParts { edges, .. } = extract_parts(
            b"10 20 50 50 re S",
            &Dict::new(),
            [0.0, 0.0, 100.0, 200.0],
            ContentCtx {
                data: &[],
                xref: None,
            },
            90,
        );
        assert_eq!(edges.len(), 4);
        // 元の下辺 (10..60, y_pdf=20 -> td180) が x=20 の縦線 y=[10..60] になる
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            20.0,
            20.0,
            10.0,
            60.0
        ));
    }

    /// re + S で4辺
    #[test]
    fn re_stroke_four_edges() {
        // 矩形 (10,20)-(60,70) → w=50 h=50。view top=200
        // PDF y: bottom-up。top-down: top = 200-y
        // 下辺 y=20 → top=180、上辺 y=70 → top=130
        // 左 x=10、右 x=60
        let edges = edges_of(b"10 20 50 50 re S", default_view());
        assert_eq!(edges.len(), 4, "expected 4 edges, got {edges:?}");

        // bottom: (10,20)-(60,20) horizontal y_end=20 → top=180
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            10.0,
            60.0,
            180.0,
            180.0
        ));
        // right: (60,20)-(60,70) vertical x_end=60
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            60.0,
            60.0,
            130.0,
            180.0
        ));
        // top: (60,70)-(10,70) horizontal y_end=70 → top=130
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            10.0,
            60.0,
            130.0,
            130.0
        ));
        // left close: (10,70)-(10,20) vertical x_end=10
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            10.0,
            10.0,
            130.0,
            180.0
        ));
    }

    /// 退化矩形 w=0 は線分と閉じ線で同一の縦辺が2本になる
    /// 重複辺は検出コアが吸収
    #[test]
    fn degenerate_rect_zero_width() {
        // re x y 0 h → move (x,y) line (x, y+h) close
        // 10 20 0 50 → 縦線 x=10, y=20..70
        let edges = edges_of(b"10 20 0 50 re S", default_view());
        assert_eq!(edges.len(), 2, "expected 2 identical edges, got {edges:?}");
        for e in &edges {
            assert_eq!(e.orientation, Orientation::Vertical);
            assert!(approx(e.left, 10.0));
            assert!(approx(e.top, 130.0));
            assert!(approx(e.bottom, 180.0));
        }
    }

    /// cm で平行移動した線分
    #[test]
    fn cm_translates_line() {
        // 0 0 m 100 0 l を (10, 20) 平行移動
        let edges = edges_of(b"1 0 0 1 10 20 cm 0 0 m 100 0 l S", default_view());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].orientation, Orientation::Horizontal);
        // 終点 y = 20 → top = 200-20 = 180
        assert!(approx(edges[0].top, 180.0));
        assert!(approx(edges[0].left, 10.0));
        assert!(approx(edges[0].right, 110.0));
    }

    /// q/Q で CTM が戻る
    #[test]
    fn q_q_restores_ctm() {
        // q 内で平行移動して線、Q 後に別の線
        let contents = b"\
q
1 0 0 1 50 0 cm
0 100 m 10 100 l S
Q
0 50 m 10 50 l S
";
        let edges = edges_of(contents, default_view());
        assert_eq!(edges.len(), 2, "got {edges:?}");
        // 1本目: x=50..60, y=100 → top=100
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            50.0,
            60.0,
            100.0,
            100.0
        ));
        // 2本目: x=0..10, y=50 → top=150（CTM が戻っている）
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            0.0,
            10.0,
            150.0,
            150.0
        ));
    }

    /// 塗りの三角形で暗黙閉じ線が辺化（軸平行分のみ）
    #[test]
    fn fill_triangle_implicit_close() {
        // (0,0) → (10,0) → (10,10) → 暗黙閉じ (10,10)→(0,0) は斜め → 捨て
        // 辺: 水平 (0,0)-(10,0)、垂直 (10,0)-(10,10)
        let edges = edges_of(b"0 0 m 10 0 l 10 10 l f", default_view());
        assert_eq!(edges.len(), 2, "got {edges:?}");
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            0.0,
            10.0,
            200.0,
            200.0
        ));
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            10.0,
            10.0,
            190.0,
            200.0
        ));
    }

    /// 塗りの軸平行閉じ: 3点で暗黙閉じが水平になる
    #[test]
    fn fill_implicit_close_axis() {
        // (0,0)-(10,0)-(10,0) wait: (0,5)-(10,5)-(10,15) close implicit (10,15)-(0,5) 斜め
        // (0,0)-(20,0)-(20,10)-(0,10) without h, fill → 暗黙閉じが左縦辺
        let edges = edges_of(b"0 0 m 20 0 l 20 10 l 0 10 l f", default_view());
        // 3 明示辺 + 1 暗黙閉じ = 4
        assert_eq!(edges.len(), 4, "got {edges:?}");
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            0.0,
            0.0,
            190.0,
            200.0
        ));
    }

    /// 斜め線は捨て、EPS 以内のズレは軸平行
    #[test]
    fn diagonal_dropped_eps_accepted() {
        // 斜め
        let edges = edges_of(b"0 0 m 10 10 l S", default_view());
        assert!(edges.is_empty());

        // dy=0.5 < 0.8 → horizontal
        let edges = edges_of(b"0 0 m 10 0.5 l S", default_view());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].orientation, Orientation::Horizontal);
        // 終点 y=0.5 → top = 200-0.5 = 199.5
        assert!(approx(edges[0].top, 199.5));

        // dx=0.5 < 0.8 → vertical
        let edges = edges_of(b"0 0 m 0.5 10 l S", default_view());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].orientation, Orientation::Vertical);
    }

    /// 角丸矩形は軸平行辺が支配的なので graphics に登録しない
    #[test]
    fn rounded_rect_not_graphic() {
        // 100x55 の角丸矩形 r=5。四隅のみ曲線で辺は軸平行
        let body = b"10 60 m 10 15 l 10 12 12 10 15 10 c 105 10 l 108 10 110 12 110 15 c \
                     110 60 l 110 63 108 65 105 65 c 15 65 l 12 65 10 63 10 60 c h f";
        let parts = parts_of(body, default_view());
        assert!(parts.graphics.is_empty(), "got {:?}", parts.graphics);
        assert_eq!(parts.edges.len(), 4, "got {:?}", parts.edges);
    }

    /// 曲線・斜め線が主体のパスは従来どおり graphics に登録する
    #[test]
    fn curve_and_diagonal_paths_still_graphic() {
        let parts = parts_of(b"10 10 m 30 40 50 40 70 40 c 90 40 110 10 130 10 c S", default_view());
        assert_eq!(parts.graphics.len(), 1, "got {:?}", parts.graphics);
        let g = &parts.graphics[0];
        assert!(approx(g.left, 10.0) && approx(g.right, 130.0));
        assert!(approx(g.top, 160.0) && approx(g.bottom, 190.0));
        assert!(g.curve_len > 100.0, "curve_len={}", g.curve_len);

        let parts = parts_of(b"0 0 m 100 100 l S", default_view());
        assert_eq!(parts.graphics.len(), 1, "got {:?}", parts.graphics);
    }

    /// 太さが許容差以下の塗り罫線は短辺を含んでも graphics に登録しない
    #[test]
    fn thin_filled_rule_not_graphic() {
        let parts = parts_of(b"10 100 100 0.8 re f", default_view());
        assert!(parts.graphics.is_empty(), "got {:?}", parts.graphics);
        // 長辺2本は従来どおり辺化される
        assert_eq!(parts.edges.len(), 2, "got {:?}", parts.edges);
    }

    /// n は辺にしない
    #[test]
    fn end_path_n_no_edges() {
        let edges = edges_of(b"0 0 100 50 re n", default_view());
        assert!(edges.is_empty());
        // W n も同様
        let edges = edges_of(b"0 0 100 50 re W n", default_view());
        assert!(edges.is_empty());
    }

    /// Form XObject の Matrix 合成と再帰
    #[test]
    fn form_xobject_matrix_and_recurse() {
        // Form 本文: 原点から水平線 0 0 m 20 0 l S
        // Matrix: 平行移動 (30, 40)
        let form_body = b"0 0 m 20 0 l S";
        let mut form_dict = Dict::new();
        form_dict.set("Subtype", Object::Name("Form".into()));
        form_dict.set(
            "Matrix",
            Object::Array(vec![
                Object::Int(1),
                Object::Int(0),
                Object::Int(0),
                Object::Int(1),
                Object::Int(30),
                Object::Int(40),
            ]),
        );
        form_dict.set("Length", Object::Int(form_body.len() as i64));
        let stream = Stream::new(form_dict, 0, form_body.len());

        let mut xo = Dict::new();
        xo.set("Fm1", Object::Stream(stream));
        let mut resources = Dict::new();
        resources.set("XObject", Object::Dict(xo));

        let edges = extract_edges(
            b"/Fm1 Do",
            &resources,
            default_view(),
            ContentCtx {
                data: form_body,
                xref: None,
            },
        );
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].orientation, Orientation::Horizontal);
        // 変換後 (30,40)-(50,40)、top = 200-40 = 160
        assert!(approx(edges[0].left, 30.0));
        assert!(approx(edges[0].right, 50.0));
        assert!(approx(edges[0].top, 160.0));
    }

    /// inline Form の自己参照が深さ上限で打ち切られスタックが尽きない
    #[test]
    fn form_xobject_inline_self_reference_bounded() {
        // Form 本文が自身を Do する inline Form
        let form_body = b"/Fm1 Do";
        let mut form_dict = Dict::new();
        form_dict.set("Subtype", Object::Name("Form".into()));
        form_dict.set("Length", Object::Int(form_body.len() as i64));
        let stream = Stream::new(form_dict, 0, form_body.len());

        let mut xo = Dict::new();
        xo.set("Fm1", Object::Stream(stream));
        let mut resources = Dict::new();
        resources.set("XObject", Object::Dict(xo));

        let edges = extract_edges(
            b"/Fm1 Do",
            &resources,
            default_view(),
            ContentCtx {
                data: form_body,
                xref: None,
            },
        );
        assert!(edges.is_empty());
    }

    /// inline Form の深いネストが深さ上限で打ち切られスタックが尽きない
    #[test]
    fn form_xobject_inline_deep_chain_bounded() {
        // Fm1..Fm25 の inline Form を連結
        // Fm_i 本文が Fm_{i+1} Do
        // Fm25 本文は空
        let n: usize = MAX_FORM_DEPTH + 5;
        let mut data: Vec<u8> = Vec::new();
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        for i in 1..n {
            let start = data.len();
            let body = format!("/Fm{} Do", i + 1);
            data.extend_from_slice(body.as_bytes());
            ranges.push((start, body.len()));
        }
        let last_start = data.len();
        ranges.push((last_start, 0));

        let mut xo = Dict::new();
        for (i, (start, len)) in ranges.iter().enumerate() {
            let mut form_dict = Dict::new();
            form_dict.set("Subtype", Object::Name("Form".into()));
            form_dict.set("Length", Object::Int(*len as i64));
            let stream = Stream::new(form_dict, *start, *len);
            xo.set(format!("Fm{}", i + 1), Object::Stream(stream));
        }
        let mut resources = Dict::new();
        resources.set("XObject", Object::Dict(xo));

        let edges = extract_edges(
            b"/Fm1 Do",
            &resources,
            default_view(),
            ContentCtx {
                data: &data,
                xref: None,
            },
        );
        assert!(edges.is_empty());
    }

    /// BI...EI スキップ後の演算子が処理される
    #[test]
    fn bi_ei_skip_then_path() {
        // インライン画像の後に水平線
        let contents = b"BI /W 1 /H 1 /CS /G ID \x00 EI 0 100 m 50 100 l S";
        let edges = edges_of(contents, default_view());
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].orientation, Orientation::Horizontal);
        assert!(approx(edges[0].left, 0.0));
        assert!(approx(edges[0].right, 50.0));
        assert!(approx(edges[0].top, 100.0));
    }

    /// view 原点が非ゼロのときの left/top
    #[test]
    fn view_origin_nonzero() {
        // view = [10, 20, 210, 220] → 原点左上 PDF (10, 220)
        // 線 (30, 40)-(80, 40)
        // left = 30-10 = 20, right = 80-10 = 70
        // top = 220-40 = 180
        let view = [10.0, 20.0, 210.0, 220.0];
        let edges = edges_of(b"30 40 m 80 40 l S", view);
        assert_eq!(edges.len(), 1);
        assert!(approx(edges[0].left, 20.0));
        assert!(approx(edges[0].right, 70.0));
        assert!(approx(edges[0].top, 180.0));
        assert!(approx(edges[0].bottom, 180.0));

        // 縦線 (30, 40)-(30, 90)
        // left = 30-10 = 20
        // top = 220-90 = 130, bottom = 220-40 = 180
        let edges = edges_of(b"30 40 m 30 90 l S", view);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].orientation, Orientation::Vertical);
        assert!(approx(edges[0].left, 20.0));
        assert!(approx(edges[0].top, 130.0));
        assert!(approx(edges[0].bottom, 180.0));
    }

    /// stroke のみでは暗黙閉じしない
    #[test]
    fn stroke_no_implicit_close() {
        let edges = edges_of(b"0 0 m 20 0 l 20 10 l 0 10 l S", default_view());
        // 3 辺のみ（左辺は閉じなし）
        assert_eq!(edges.len(), 3, "got {edges:?}");
    }

    // -----------------------------------------------------------------------
    // テキスト（glyphs）
    // -----------------------------------------------------------------------

    fn glyphs_of(contents: &[u8], view: [f64; 4]) -> Vec<GlyphPart> {
        extract_parts(
            contents,
            &Dict::new(),
            view,
            ContentCtx {
                data: &[],
                xref: None,
            },
            0,
        )
        .glyphs
    }

    /// スタブ: 幅 500/1000、fontSize に対する字送り = 0.5 * fontSize（hScale=1）
    fn stub_advance(font_size: f64) -> f64 {
        0.5 * font_size
    }

    /// Td / Tm での原点移動と bbox の位置
    #[test]
    fn text_td_tm_origin_and_bbox() {
        // Tm で (100, 50) に置き "A"、fontSize=20
        // 原点 PDF (100, 50)、幅 10、ascent=0.88 descent=-0.12
        let contents = b"BT /F1 20 Tf 1 0 0 1 100 50 Tm (A) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ch, 'A');
        assert!(approx(g[0].left, 100.0), "left={}", g[0].left);
        assert!(approx(g[0].right, 110.0), "right={}", g[0].right);
        // top = 200 - (50 + 0.88*20) = 200 - 67.6 = 132.4
        // bottom = 200 - (50 + (-0.12)*20) = 200 - 47.6 = 152.4
        assert!(approx(g[0].top, 132.4), "top={}", g[0].top);
        assert!(approx(g[0].bottom, 152.4), "bottom={}", g[0].bottom);

        // Td で (30, 40) 移動（BT 後の単位行列から）
        let contents = b"BT /F1 10 Tf 30 40 Td (B) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ch, 'B');
        assert!(approx(g[0].left, 30.0));
        assert!(approx(g[0].right, 30.0 + stub_advance(10.0)));
        // baseline y=40, top = 200 - (40+8.8) = 151.2
        assert!(approx(g[0].top, 151.2));
        assert!(approx(g[0].bottom, 200.0 - (40.0 - 1.2)));
    }

    /// TJ の数値調整が次グリフの x に効く
    #[test]
    fn text_tj_number_shifts_next_glyph() {
        // [(A) 200 (B)] fontSize=10 → A 幅 5、数値 200 → extra = -200/1000*10 = -2
        // B の left = 0 + 5 + (-2) = 3
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 100 Tm [(A) 200 (B)] TJ ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 2, "got {g:?}");
        assert_eq!(g[0].ch, 'A');
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 5.0));
        assert_eq!(g[1].ch, 'B');
        assert!(approx(g[1].left, 3.0), "left={}", g[1].left);
        assert!(approx(g[1].right, 8.0), "right={}", g[1].right);
    }

    /// Tc / Tz の字送り、Tw が空白のみ
    #[test]
    fn text_tc_tz_tw_advance() {
        // Tc=2, fontSize=10, "AB" → each glyph advance = 5 + 2 = 7
        let contents = b"BT /F1 10 Tf 2 Tc 1 0 0 1 0 50 Tm (AB) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 2);
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 7.0), "right={}", g[0].right);
        assert!(approx(g[1].left, 7.0));
        assert!(approx(g[1].right, 14.0));

        // 表経路の bbox は Tc 込みのまま（公称幅分離の前提）
        let (_, _, _, chars) = text_of(contents, default_view());
        assert_eq!(chars.len(), 2);
        assert!(approx(chars[0].glyph_width.unwrap_or(-1.0), 5.0));
        assert!(approx(chars[0].right - chars[0].left, 7.0));
        assert!(approx(chars[1].glyph_width.unwrap_or(-1.0), 5.0));

        // Tz 50% → hScale=0.5, scaledDim = 0.5*10*0.5 = 2.5, Tc=0
        let contents = b"BT /F1 10 Tf 50 Tz 1 0 0 1 0 50 Tm (A) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 2.5), "right={}", g[0].right);

        // Tw=4, "A B" → A advance 5, space advance 5+4=9, B after that
        let contents = b"BT /F1 10 Tf 4 Tw 1 0 0 1 0 50 Tm (A B) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 3);
        assert_eq!(g[0].ch, 'A');
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 5.0)); // Tw は空白以外に効かない
        assert_eq!(g[1].ch, ' ');
        assert!(approx(g[1].left, 5.0));
        assert!(approx(g[1].right, 14.0), "space right={}", g[1].right);
        assert_eq!(g[2].ch, 'B');
        assert!(approx(g[2].left, 14.0));
    }

    /// 語間ギャップへの空白合成
    #[test]
    fn word_space_synthesis() {
        // "AB"(0..10) と "CD"(13..23): ギャップ3 = 字高10 の 0.3 倍 → 空白1つ合成
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (AB) Tj 1 0 0 1 13 50 Tm (CD) Tj ET";
        let g = glyphs_of(contents, default_view());
        let sp: Vec<_> = g.iter().filter(|x| x.ch == ' ').collect();
        assert_eq!(sp.len(), 1, "got {g:?}");
        assert!(approx(sp[0].left, 10.0), "left={}", sp[0].left);
        assert!(approx(sp[0].right, 13.0), "right={}", sp[0].right);

        // ギャップ15 > 字高 → 列間とみなし合成しない
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (AB) Tj 1 0 0 1 25 50 Tm (CD) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert!(g.iter().all(|x| x.ch != ' '), "got {g:?}");

        // 実在する空白の上には重ねて合成しない
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (A B) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.iter().filter(|x| x.ch == ' ').count(), 1, "got {g:?}");
    }

    /// q/Q でテキスト状態（Tw 等）が保存・復元される
    #[test]
    fn text_state_restored_by_grestore() {
        // q 内で Tw=100 を設定し Q で復元 → 以降の空白に Tw が効かない
        let contents = b"q BT /F1 10 Tf 100 Tw ET Q BT /F1 10 Tf 1 0 0 1 0 50 Tm (A B) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 3, "got {g:?}");
        assert_eq!(g[1].ch, ' ');
        assert!(
            approx(g[1].right - g[1].left, 5.0),
            "space width={}",
            g[1].right - g[1].left
        );
    }

    /// ' と " の改行・設定
    #[test]
    fn text_quote_operators() {
        // TL=12, Td to (10,100), "A" then ' "B" → B on next line y=100-12=88
        let contents = b"BT /F1 10 Tf 12 TL 10 100 Td (A) Tj (B) ' ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].ch, 'A');
        // baseline 100 → bottom = 200 - (100 - 1.2) = 101.2
        assert!(approx(g[0].left, 10.0));
        assert!(approx(g[0].bottom, 101.2), "bottom={}", g[0].bottom);
        assert_eq!(g[1].ch, 'B');
        // baseline 88 → bottom = 200 - (88 - 1.2) = 113.2
        assert!(approx(g[1].left, 10.0));
        assert!(approx(g[1].bottom, 113.2), "bottom={}", g[1].bottom);

        // " : 3 1 (C) " → Tw=3 Tc=1, next line, show C
        // after previous? standalone: BT, TL, Td, then 3 1 (C) "
        let contents = b"BT /F1 10 Tf 12 TL 0 100 Td 3 1 (C) \" ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ch, 'C');
        // next line: y=100-12=88, advance = 5 + 1 = 6 (Tc=1)
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 6.0), "right={}", g[0].right);
        assert!(approx(g[0].bottom, 113.2));
    }

    /// cm とテキスト行列の合成（拡大時の bbox）
    #[test]
    fn text_cm_scales_bbox() {
        // cm で 2 倍拡大、Tm 原点 (10, 20)、fontSize=10 → device font 20, advance 10
        let contents = b"2 0 0 2 0 0 cm BT /F1 10 Tf 1 0 0 1 10 20 Tm (A) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        // origin device = (20, 40), advance device = 10 (0.5*10*2)
        assert!(approx(g[0].left, 20.0), "left={}", g[0].left);
        assert!(approx(g[0].right, 30.0), "right={}", g[0].right);
        // fontSize_dev = hypot(trm[2],trm[3]) = 20
        // top = 200 - (40 + 0.88*20) = 200 - 57.6 = 142.4
        assert!(approx(g[0].top, 142.4), "top={}", g[0].top);
        assert!(approx(g[0].bottom, 200.0 - (40.0 - 0.12 * 20.0)));
    }

    /// 制御文字スキップと空白グリフ出力
    #[test]
    fn text_control_skip_space_emit() {
        // \n \r \t は出力しない、スペースは出す
        // PDF literal string: (A\n \tB) — lexer がどう扱うか
        // バイト列で直接: A, 0x0a, space, 0x09, B
        let mut contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (".to_vec();
        contents.extend_from_slice(b"A");
        contents.push(b'\n');
        contents.push(b' ');
        contents.push(b'\t');
        contents.extend_from_slice(b"B) Tj ET");
        let g = glyphs_of(&contents, default_view());
        let chars: Vec<char> = g.iter().map(|x| x.ch).collect();
        assert_eq!(chars, vec!['A', ' ', 'B'], "got {chars:?}");
        // 制御文字分も字送りは進むので B の位置は A + \n + space + \t の後
        // 4 advances of 5 after A before B? glyphs: A, \n(skip), space, \t(skip), B
        // advances: A, \n, space, \t, then B at 4*5=20
        assert!(approx(g[2].left, 20.0), "B left={}", g[2].left);
    }

    /// ascent/descent 由来の top/bottom（スタブ値）
    #[test]
    fn text_ascent_descent_bbox() {
        let contents = b"BT /F1 100 Tf 1 0 0 1 0 0 Tm (X) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        // baseline 0, fs=100, asc=0.88 desc=-0.12
        // top = 200 - 88 = 112, bottom = 200 - (-12) = 212
        assert!(approx(g[0].top, 112.0));
        assert!(approx(g[0].bottom, 212.0));
        // 高さ 100
        assert!(approx(g[0].bottom - g[0].top, 100.0));
    }

    /// view 原点が非ゼロのときの座標変換
    #[test]
    fn text_view_origin_nonzero() {
        // view = [10, 20, 210, 220], 原点左上 PDF (10, 220)
        // glyph at Tm (30, 40), fs=10
        let view = [10.0, 20.0, 210.0, 220.0];
        let contents = b"BT /F1 10 Tf 1 0 0 1 30 40 Tm (Z) Tj ET";
        let g = glyphs_of(contents, view);
        assert_eq!(g.len(), 1);
        // left = 30-10 = 20, right = 35-10 = 25
        assert!(approx(g[0].left, 20.0));
        assert!(approx(g[0].right, 25.0));
        // top = 220 - (40 + 8.8) = 171.2
        // bottom = 220 - (40 - 1.2) = 181.2
        assert!(approx(g[0].top, 171.2), "top={}", g[0].top);
        assert!(approx(g[0].bottom, 181.2), "bottom={}", g[0].bottom);
    }

    // -----------------------------------------------------------------------
    // テキストAPI（extract_text）
    // -----------------------------------------------------------------------

    fn text_of(contents: &[u8], view: [f64; 4]) -> (f64, f64, Vec<TextFont>, Vec<TextChar>) {
        let t = extract_text(
            contents,
            &Dict::new(),
            view,
            ContentCtx {
                data: &[],
                xref: None,
            },
            0,
        );
        (t.width, t.height, t.fonts, t.chars)
    }

    fn rotated_synth_space(rot: i32) -> (f64, f64, Vec<TextChar>) {
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (AB) Tj 1 0 0 1 13 50 Tm (CD) Tj ET";
        let ExtractedText {
            width: w,
            height: h,
            chars,
            ..
        } = extract_text(
            contents,
            &Dict::new(),
            [0.0, 0.0, 100.0, 200.0],
            ContentCtx {
                data: &[],
                xref: None,
            },
            rot,
        );
        let texts: Vec<&str> = chars.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["A", "B", " ", "C", "D"], "got {texts:?}");
        assert!(chars[2].synthetic);
        (w, h, chars)
    }

    /// 座標・前進・フォント表・transform の基本
    #[test]
    fn extract_text_bbox_advance_font_transform() {
        let contents = b"BT /F1 10 Tf 1 0 0 1 30 40 Tm (B) Tj ET";
        let (w, h, fonts, chars) = text_of(contents, default_view());
        assert!(approx(w, 200.0));
        assert!(approx(h, 200.0));
        assert_eq!(fonts.len(), 1);
        assert!(approx(fonts[0].ascent, 0.88));
        assert!(approx(fonts[0].descent, -0.12));
        assert!(!fonts[0].vertical);
        assert_eq!(chars.len(), 1);
        let c = &chars[0];
        assert_eq!(c.text, "B");
        assert!(!c.synthetic);
        assert_eq!(c.font, 0);
        assert!(approx(c.left, 30.0));
        assert!(approx(c.right, 30.0 + stub_advance(10.0)));
        assert!(approx(c.font_size, 10.0));
        assert!(
            approx(c.advance[0], stub_advance(10.0)),
            "adv={:?}",
            c.advance
        );
        assert!(approx(c.advance[1], 0.0));
        // TRM 表示: e=30, f=200-40=160、縦軸長=10
        assert!(approx(c.transform[4], 30.0), "e={}", c.transform[4]);
        assert!(approx(c.transform[5], 160.0), "f={}", c.transform[5]);
        // a = fontSize * hScale = 10, d = fontSize = 10（y 反転後 d は -10）
        assert!(approx(c.transform[0], 10.0), "a={}", c.transform[0]);
        assert!(approx(c.transform[3], -10.0), "d={}", c.transform[3]);
        assert!(c.upright);
        assert_eq!(c.rot, 0);
    }

    /// 合成空白がストリーム上の直前グリフ直後に入る
    #[test]
    fn extract_text_synth_space_stream_order() {
        // "AB"(0..10) と "CD"(13..23): ギャップ3 → 合成空白を B の直後へ
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (AB) Tj 1 0 0 1 13 50 Tm (CD) Tj ET";
        let (_, _, _, chars) = text_of(contents, default_view());
        let texts: Vec<&str> = chars.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["A", "B", " ", "C", "D"], "got {texts:?}");
        assert!(chars[2].synthetic);
        assert!(!chars[0].synthetic);
        assert!(approx(chars[2].left, 10.0));
        assert!(approx(chars[2].right, 13.0));
        assert_eq!(chars[2].transform, [10.0, 0.0, 0.0, -10.0, 10.0, 150.0]);
    }

    /// ページ /Rotate 90 でも合成空白の位置と TRM が表示座標に一致する
    #[test]
    fn extract_text_synth_space_page_rotate_90() {
        let (w, h, chars) = rotated_synth_space(90);
        let space = &chars[2];
        assert!(approx(w, 200.0));
        assert!(approx(h, 100.0));
        assert!(approx(space.left, 48.8));
        assert!(approx(space.right, 58.8));
        assert!(approx(space.top, 10.0));
        assert!(approx(space.bottom, 13.0));
        assert_eq!(space.transform, [0.0, 10.0, 10.0, 0.0, 50.0, 10.0]);
        assert_eq!(space.advance, [0.0, 3.0]);
        assert_eq!(space.rot, 270);
        assert!(!space.upright);
    }

    /// ページ /Rotate 180 でも合成空白の位置と TRM が表示座標に一致する
    #[test]
    fn extract_text_synth_space_page_rotate_180() {
        let (w, h, chars) = rotated_synth_space(180);
        let space = &chars[2];
        assert!(approx(w, 100.0));
        assert!(approx(h, 200.0));
        assert!(approx(space.left, 87.0));
        assert!(approx(space.right, 90.0));
        assert!(approx(space.top, 48.8));
        assert!(approx(space.bottom, 58.8));
        assert_eq!(space.transform, [-10.0, 0.0, 0.0, 10.0, 90.0, 50.0]);
        assert_eq!(space.advance, [-3.0, 0.0]);
        assert_eq!(space.rot, 180);
        assert!(!space.upright);
    }

    /// ページ /Rotate 270 でも合成空白の位置と TRM が表示座標に一致する
    #[test]
    fn extract_text_synth_space_page_rotate_270() {
        let (w, h, chars) = rotated_synth_space(270);
        let space = &chars[2];
        assert!(approx(w, 200.0));
        assert!(approx(h, 100.0));
        assert!(approx(space.left, 141.2));
        assert!(approx(space.right, 151.2));
        assert!(approx(space.top, 87.0));
        assert!(approx(space.bottom, 90.0));
        assert_eq!(space.transform, [0.0, -10.0, -10.0, 0.0, 150.0, 90.0]);
        assert_eq!(space.advance, [0.0, -3.0]);
        assert_eq!(space.rot, 90);
        assert!(!space.upright);
    }

    /// TJ 数値が前進ベクトルに反映される
    #[test]
    fn extract_text_tj_in_advance() {
        // [(A) 200 (B)] fontSize=10 → A の advance に extra=-2 が乗る
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 100 Tm [(A) 200 (B)] TJ ET";
        let (_, _, _, chars) = text_of(contents, default_view());
        assert_eq!(chars.len(), 2);
        // A: bbox_advance=5, extra=-2 → advance=3
        assert!(
            approx(chars[0].advance[0], 3.0),
            "adv0={:?}",
            chars[0].advance
        );
        assert!(approx(chars[1].left, 3.0));
        assert!(approx(chars[1].advance[0], 5.0));
        // 公称幅は TJ・Tc を含まない
        assert!(approx(chars[0].glyph_width.unwrap_or(-1.0), 5.0));
        assert!(approx(chars[1].glyph_width.unwrap_or(-1.0), 5.0));
    }

    /// Tc で広げ TJ で戻す詰め配置でも公称幅は pure、bbox は Tc 込み
    #[test]
    fn extract_text_tc_pack_nominal_width() {
        // stub 幅 500 → pure=5。Tc=3、TJ 300（extra=-3）→ 原点間隔 5、bbox 幅 8
        let contents = b"BT /F1 10 Tf 3 Tc 1 0 0 1 0 50 Tm [(A) 300 (B)] TJ ET";
        let (_, _, _, chars) = text_of(contents, default_view());
        let solids: Vec<_> = chars.iter().filter(|c| !c.synthetic).collect();
        assert_eq!(solids.len(), 2, "chars={chars:?}");
        assert!(approx(solids[0].left, 0.0));
        assert!(approx(solids[0].right, 8.0), "right={}", solids[0].right);
        assert!(approx(solids[0].glyph_width.unwrap_or(-1.0), 5.0));
        assert!(approx(solids[1].left, 5.0), "B left={}", solids[1].left);
        assert!(approx(solids[1].glyph_width.unwrap_or(-1.0), 5.0));
        // 表グリフも占有幅のまま
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 2);
        assert!(approx(g[0].right - g[0].left, 8.0));
        assert!(approx(g[1].left, 5.0));
    }

    /// ページ /Rotate 90 で width/height と座標が入れ替わる
    #[test]
    fn extract_text_page_rotate_90() {
        // 非正方形 view: 100×200
        let view = [0.0, 0.0, 100.0, 200.0];
        let contents = b"BT /F1 10 Tf 1 0 0 1 10 20 Tm (A) Tj ET";
        let ExtractedText {
            width: w,
            height: h,
            chars,
            ..
        } = extract_text(
            contents,
            &Dict::new(),
            view,
            ContentCtx {
                data: &[],
                xref: None,
            },
            90,
        );
        // view 100x200 → 90度で 200x100
        assert!(approx(w, 200.0), "w={w}");
        assert!(approx(h, 100.0), "h={h}");
        assert_eq!(chars.len(), 1);
        let c = &chars[0];
        assert!(c.left.is_finite() && c.right.is_finite());
        assert!(c.transform[4].is_finite() && c.transform[5].is_finite());
        // 表示座標上の bbox がページ内に収まる
        assert!(c.left >= -1.0 && c.right <= w + 1.0);
        assert!(c.top >= -1.0 && c.bottom <= h + 1.0);
    }

    // -----------------------------------------------------------------------
    // 縦書き（Identity-V）
    // -----------------------------------------------------------------------

    /// 指定した縦メトリクスを持つ Identity-V の Type0 resources
    fn vertical_resources_with_metrics(dw2: Option<Vec<Object>>, w2: Option<Vec<Object>>) -> Dict {
        let mut cid = Dict::new();
        cid.set("Subtype", Object::Name("CIDFontType2".into()));
        cid.set("DW", Object::Int(1000));
        if let Some(dw2) = dw2 {
            cid.set("DW2", Object::Array(dw2));
        }
        if let Some(w2) = w2 {
            cid.set("W2", Object::Array(w2));
        }
        let mut type0 = Dict::new();
        type0.set("Subtype", Object::Name("Type0".into()));
        type0.set("Encoding", Object::Name("Identity-V".into()));
        type0.set("DescendantFonts", Object::Array(vec![Object::Dict(cid)]));
        let mut fonts = Dict::new();
        fonts.set("F1", Object::Dict(type0));
        let mut res = Dict::new();
        res.set("Font", Object::Dict(fonts));
        res
    }

    /// Identity-V の Type0 を /F1 に載せた resources
    fn vertical_resources(with_w2: bool) -> Dict {
        let w2 = with_w2.then(|| {
            vec![
                Object::Int(0x41),
                Object::Array(vec![Object::Int(-500), Object::Int(500), Object::Int(880)]),
            ]
        });
        vertical_resources_with_metrics(Some(vec![Object::Int(880), Object::Int(-1000)]), w2)
    }

    fn text_of_res(
        contents: &[u8],
        resources: &Dict,
        view: [f64; 4],
    ) -> (f64, f64, Vec<TextFont>, Vec<TextChar>) {
        let t = extract_text(
            contents,
            resources,
            view,
            ContentCtx {
                data: &[],
                xref: None,
            },
            0,
        );
        (t.width, t.height, t.fonts, t.chars)
    }

    /// 縦書き: 前進が縦方向、bbox が縦に並ぶ、TextFont.vertical
    #[test]
    fn extract_text_vertical_advance_and_stack() {
        // Identity-V: <00410042> = CID 0x41, 0x42
        // DW2 既定 w1y=-1000、fs=10 → 各字の PDF y 前進 = -10
        let contents = b"BT /F1 10 Tf 1 0 0 1 50 150 Tm <00410042> Tj ET";
        let res = vertical_resources(false);
        let (_, _, fonts, chars) = text_of_res(contents, &res, default_view());
        assert_eq!(fonts.len(), 1);
        assert!(fonts[0].vertical);
        assert_eq!(chars.len(), 2);

        // advance は表示座標で下向き（+y）
        assert!(
            approx(chars[0].advance[0], 0.0),
            "adv0={:?}",
            chars[0].advance
        );
        assert!(
            approx(chars[0].advance[1], 10.0),
            "adv0 y={:?}",
            chars[0].advance
        );
        assert!(approx(chars[1].advance[0], 0.0));
        assert!(approx(chars[1].advance[1], 10.0));

        // 横位置は概ね同じ列、縦位置は下へ並ぶ
        assert!(
            (chars[0].left - chars[1].left).abs() < 1.0,
            "lefts {} {}",
            chars[0].left,
            chars[1].left
        );
        assert!(
            chars[1].top > chars[0].top + 5.0,
            "tops {} {}",
            chars[0].top,
            chars[1].top
        );
    }

    /// /W2 個別値が /DW2 既定より優先され前進量が変わる
    #[test]
    fn extract_text_vertical_w2_vs_dw2() {
        let contents = b"BT /F1 10 Tf 1 0 0 1 50 150 Tm <00410042> Tj ET";
        let res = vertical_resources(true);
        let (_, _, _, chars) = text_of_res(contents, &res, default_view());
        assert_eq!(chars.len(), 2);
        // CID 0x41: w1y=-500 → advance y = 5
        assert!(
            approx(chars[0].advance[1], 5.0),
            "w2 adv={:?}",
            chars[0].advance
        );
        // CID 0x42: 既定 w1y=-1000 → advance y = 10
        assert!(
            approx(chars[1].advance[1], 10.0),
            "dw2 adv={:?}",
            chars[1].advance
        );
        // 2 字目の top 差は 5（W2）分だけ
        let dy = chars[1].top - chars[0].top;
        assert!(approx(dy, 5.0), "dy={dy}");
    }

    /// CTM の拡大を縦書き bbox の幅と高さへ同じ倍率で反映する
    #[test]
    fn extract_text_vertical_bbox_respects_ctm_scale() {
        let contents = b"2 0 0 2 0 0 cm BT /F1 10 Tf 1 0 0 1 50 150 Tm <0041> Tj ET";
        let res = vertical_resources(false);
        let (_, _, _, chars) = text_of_res(contents, &res, [0.0, 0.0, 400.0, 400.0]);
        assert_eq!(chars.len(), 1);
        let width = chars[0].right - chars[0].left;
        let height = chars[0].bottom - chars[0].top;
        assert!(approx(width, 20.0), "width={width}");
        assert!(approx(height, 20.0), "height={height}");
    }

    /// 負の Tz でも横軸の鏡映を二重に反転しない
    #[test]
    fn extract_text_vertical_bbox_negative_hscale() {
        let contents = b"BT /F1 10 Tf -100 Tz 1 0 0 1 50 100 Tm <0041> Tj ET";
        let res = vertical_resources(false);
        let (_, _, _, chars) = text_of_res(contents, &res, default_view());
        assert_eq!(chars.len(), 1);
        assert!(approx(chars[0].left, 45.0), "left={}", chars[0].left);
        assert!(approx(chars[0].right, 55.0), "right={}", chars[0].right);
    }

    /// 縦書きの Tc・2バイト空白への Tw・TJ 数値を前進量へ反映する
    #[test]
    fn extract_text_vertical_tc_tw_and_tj_advance() {
        let contents = b"BT /F1 10 Tf 1 Tc 2 Tw 1 0 0 1 50 150 Tm [<00410020> 200 <0042>] TJ ET";
        let res = vertical_resources(false);
        let (_, _, _, chars) = text_of_res(contents, &res, default_view());
        let texts: Vec<&str> = chars.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["A", " ", "B"]);
        // A: -10 - (-Tc) = -9
        assert!(approx(chars[0].advance[1], 9.0), "A={:?}", chars[0].advance);
        // 空白: -10 - (-Tc + Tw + TJ 2) = -13
        assert!(
            approx(chars[1].advance[1], 13.0),
            "space={:?}",
            chars[1].advance
        );
        assert!(approx(chars[2].advance[1], 9.0), "B={:?}", chars[2].advance);
        assert!(approx(chars[1].transform[5] - chars[0].transform[5], 9.0));
        assert!(approx(chars[2].transform[5] - chars[1].transform[5], 13.0));
    }

    /// v1x/v1y の個別値で bbox の絶対位置をずらす
    #[test]
    fn extract_text_vertical_custom_origin_bbox_position() {
        let res = vertical_resources_with_metrics(
            Some(vec![Object::Int(880), Object::Int(-1000)]),
            Some(vec![
                Object::Int(0x41),
                Object::Array(vec![Object::Int(-1000), Object::Int(200), Object::Int(300)]),
            ]),
        );
        let contents = b"BT /F1 10 Tf 1 0 0 1 50 100 Tm <0041> Tj ET";
        let (_, _, _, chars) = text_of_res(contents, &res, default_view());
        assert_eq!(chars.len(), 1);
        assert!(approx(chars[0].left, 48.0), "left={}", chars[0].left);
        assert!(approx(chars[0].right, 58.0), "right={}", chars[0].right);
        assert!(approx(chars[0].top, 88.2), "top={}", chars[0].top);
        assert!(approx(chars[0].bottom, 98.2), "bottom={}", chars[0].bottom);
    }

    /// 縦書き parts: グリフ bbox が縦に並ぶ
    #[test]
    fn glyphs_vertical_stack() {
        let contents = b"BT /F1 10 Tf 1 0 0 1 50 150 Tm <00410042> Tj ET";
        let res = vertical_resources(false);
        let ExtractedParts { glyphs, .. } = extract_parts(
            contents,
            &res,
            default_view(),
            ContentCtx {
                data: &[],
                xref: None,
            },
            0,
        );
        assert_eq!(glyphs.len(), 2);
        assert!((glyphs[0].left - glyphs[1].left).abs() < 1.0, "lefts");
        assert!(
            glyphs[1].top > glyphs[0].top + 5.0,
            "tops {} {}",
            glyphs[0].top,
            glyphs[1].top
        );
    }

    // -----------------------------------------------------------------------
    // フォント表の決定性・インライン辞書の重複解消
    // -----------------------------------------------------------------------

    /// 直接辞書の /Font で同じ名前の Tf を複数回 → fonts 表は1エントリ
    #[test]
    fn extract_text_inline_font_same_name_dedup() {
        let mut type1 = Dict::new();
        type1.set("Subtype", Object::Name("Type1".into()));
        type1.set("BaseFont", Object::Name("Helvetica".into()));
        let mut fonts_dict = Dict::new();
        fonts_dict.set("F1", Object::Dict(type1));
        let mut res = Dict::new();
        res.set("Font", Object::Dict(fonts_dict));

        let contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (A) Tj /F1 12 Tf 1 0 0 1 20 50 Tm (B) Tj ET";
        let (_, _, fonts, chars) = text_of_res(contents, &res, default_view());
        assert_eq!(fonts.len(), 1, "fonts={fonts:?}");
        assert_eq!(fonts[0].name, "Helvetica");
        assert_eq!(chars.len(), 2);
        assert_eq!(chars[0].font, 0);
        assert_eq!(chars[1].font, 0);
    }

    /// BaseFont 無しの同一フォントを別名で参照 → TextFont.name は最初のリソース名
    #[test]
    fn extract_text_font_name_first_resource_when_no_basefont() {
        let data = build_pdf_font_no_basefont();
        let xref = XRef::parse(&data).expect("parse");
        let mut fonts_dict = Dict::new();
        fonts_dict.set("F1", Object::Ref(Ref::new(3, 0)));
        fonts_dict.set("F2", Object::Ref(Ref::new(3, 0)));
        let mut res = Dict::new();
        res.set("Font", Object::Dict(fonts_dict));

        let contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (A) Tj /F2 12 Tf 1 0 0 1 20 50 Tm (B) Tj ET";
        let ExtractedText { fonts, chars, .. } = extract_text(
            contents,
            &res,
            default_view(),
            ContentCtx {
                data: &data,
                xref: Some(&xref),
            },
            0,
        );
        assert_eq!(fonts.len(), 1, "fonts={fonts:?}");
        assert_eq!(fonts[0].name, "F1");
        assert_eq!(chars.len(), 2);
        assert_eq!(chars[0].font, 0);
        assert_eq!(chars[1].font, 0);
    }

    /// BaseFont 無しの Type1 フォントオブジェクトを1つ持つ最小 PDF
    fn build_pdf_font_no_basefont() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.4\n");

        let o1 = body.len();
        body.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let o2 = body.len();
        body.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

        let o3 = body.len();
        body.extend_from_slice(b"3 0 obj\n<< /Type /Font /Subtype /Type1 >>\nendobj\n");

        let xref_off = body.len();
        body.extend_from_slice(b"xref\n0 4\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        body.extend_from_slice(format!("{o1:010} 00000 n \n").as_bytes());
        body.extend_from_slice(format!("{o2:010} 00000 n \n").as_bytes());
        body.extend_from_slice(format!("{o3:010} 00000 n \n").as_bytes());
        body.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n");
        body.extend_from_slice(format!("{xref_off}\n%%EOF\n").as_bytes());
        body
    }

    // -----------------------------------------------------------------------
    // 重複グリフ除去（dedup_overlaid_glyphs）
    // -----------------------------------------------------------------------

    /// 除去テスト用の基準グリフ
    fn dedup_glyph(text: &str, x: f64) -> PendingTextChar {
        PendingTextChar {
            text: text.to_string(),
            left: x,
            right: x + 5.0,
            top: 10.0,
            bottom: 20.0,
            transform: [10.0, 0.0, 0.0, -10.0, x, 30.0],
            advance: [5.0, 0.0],
            glyph_width: Some(5.0),
            font: 0,
            font_size: 10.0,
            rot_q: RotQ::R0,
            upright: true,
            synthetic: false,
        }
    }

    fn dedup_texts(chars: &mut Vec<PendingTextChar>) -> Vec<String> {
        dedup_overlaid_glyphs(chars);
        chars.iter().map(|c| c.text.clone()).collect()
    }

    /// 完全一致2個は先頭のみ残り順序が保たれる
    #[test]
    fn dedup_exact_pair_keeps_first() {
        let mut v = vec![
            dedup_glyph("A", 0.0),
            dedup_glyph("B", 5.0),
            dedup_glyph("A", 0.0),
        ];
        assert_eq!(dedup_texts(&mut v), vec!["A", "B"]);
    }

    /// 3個以上の完全一致も先頭の1個のみ残る
    #[test]
    fn dedup_triple_keeps_first() {
        let mut v = vec![
            dedup_glyph("A", 0.0),
            dedup_glyph("A", 0.0),
            dedup_glyph("A", 0.0),
        ];
        assert_eq!(dedup_texts(&mut v), vec!["A"]);
    }

    /// キーのいずれか1箇所だけが違う組は両方残る
    #[test]
    fn dedup_single_field_difference_keeps_both() {
        let variants: Vec<(&str, Box<dyn Fn(&mut PendingTextChar)>)> = vec![
            ("text", Box::new(|c| c.text = "B".into())),
            ("font", Box::new(|c| c.font = 1)),
            ("font_size", Box::new(|c| c.font_size = 11.0)),
            ("left", Box::new(|c| c.left += 0.5)),
            ("right", Box::new(|c| c.right += 0.5)),
            ("top", Box::new(|c| c.top += 0.5)),
            ("bottom", Box::new(|c| c.bottom += 0.5)),
            ("transform_a", Box::new(|c| c.transform[0] += 0.5)),
            ("origin_x", Box::new(|c| c.transform[4] += 0.001)),
            ("advance_x", Box::new(|c| c.advance[0] += 0.001)),
            ("rot_q", Box::new(|c| c.rot_q = RotQ::R180)),
            ("upright", Box::new(|c| c.upright = false)),
        ];
        for (name, mutate) in variants {
            let mut b = dedup_glyph("A", 0.0);
            mutate(&mut b);
            let mut v = vec![dedup_glyph("A", 0.0), b];
            dedup_overlaid_glyphs(&mut v);
            assert_eq!(v.len(), 2, "field {name}");
        }
    }

    /// 合字は文字列全体で比較する（完全一致は1個、分解列とは別）
    #[test]
    fn dedup_ligature_string() {
        let mut v = vec![dedup_glyph("fi", 0.0), dedup_glyph("fi", 0.0)];
        assert_eq!(dedup_texts(&mut v), vec!["fi"]);
        let mut v = vec![
            dedup_glyph("fi", 0.0),
            dedup_glyph("f", 0.0),
            dedup_glyph("i", 0.0),
        ];
        assert_eq!(dedup_texts(&mut v), vec!["fi", "f", "i"]);
    }

    /// 実空白の完全一致は先頭のみ残る（3連も同様）
    #[test]
    fn dedup_real_space() {
        let mut v = vec![
            dedup_glyph(" ", 0.0),
            dedup_glyph(" ", 0.0),
            dedup_glyph(" ", 0.0),
        ];
        assert_eq!(dedup_texts(&mut v), vec![" "]);
    }

    /// synthetic と空 text は判定に参加せず常に残る
    #[test]
    fn dedup_skips_synthetic_and_empty() {
        let mut a = dedup_glyph(" ", 0.0);
        a.synthetic = true;
        let mut b = dedup_glyph(" ", 0.0);
        b.synthetic = true;
        let mut v = vec![a, b];
        dedup_overlaid_glyphs(&mut v);
        assert_eq!(v.len(), 2);

        let mut v = vec![dedup_glyph("", 0.0), dedup_glyph("", 0.0)];
        dedup_overlaid_glyphs(&mut v);
        assert_eq!(v.len(), 2);
    }

    /// 非有限値を含むグリフは残る（font_size・bbox・transform・advance の各群）
    #[test]
    fn dedup_keeps_nonfinite() {
        let muts: Vec<Box<dyn Fn(&mut PendingTextChar)>> = vec![
            Box::new(|c| c.font_size = f64::NAN),
            Box::new(|c| c.top = f64::INFINITY),
            Box::new(|c| c.transform[2] = f64::NAN),
            Box::new(|c| c.advance[1] = f64::NEG_INFINITY),
        ];
        for (i, m) in muts.into_iter().enumerate() {
            let mut a = dedup_glyph("A", 0.0);
            m(&mut a);
            let mut b = dedup_glyph("A", 0.0);
            m(&mut b);
            let mut v = vec![a, b];
            dedup_overlaid_glyphs(&mut v);
            assert_eq!(v.len(), 2, "case {i}");
        }
    }

    /// +0.0 と -0.0 は同一視する
    #[test]
    fn dedup_zero_sign_normalized() {
        let mut a = dedup_glyph("A", 0.0);
        a.advance[1] = 0.0;
        let mut b = dedup_glyph("A", 0.0);
        b.advance[1] = -0.0;
        let mut v = vec![a, b];
        dedup_overlaid_glyphs(&mut v);
        assert_eq!(v.len(), 1);
    }

    /// 回転グリフの完全一致も1個になる
    #[test]
    fn dedup_rotated_pair() {
        let mut a = dedup_glyph("A", 0.0);
        a.rot_q = RotQ::R90;
        a.upright = false;
        let mut b = dedup_glyph("A", 0.0);
        b.rot_q = RotQ::R90;
        b.upright = false;
        let mut v = vec![a, b];
        dedup_overlaid_glyphs(&mut v);
        assert_eq!(v.len(), 1);
    }

    /// ページを跨ぐ同一キーは除去しない（走査はページ単位）
    #[test]
    fn dedup_per_page_scope() {
        let mut p1 = vec![dedup_glyph("A", 0.0)];
        let mut p2 = vec![dedup_glyph("A", 0.0)];
        dedup_overlaid_glyphs(&mut p1);
        dedup_overlaid_glyphs(&mut p2);
        assert_eq!(p1.len() + p2.len(), 2);
    }

    /// 重複なしの入力は順序・内容とも不変
    #[test]
    fn dedup_identity_without_duplicates() {
        let mut v = vec![
            dedup_glyph("A", 0.0),
            dedup_glyph("B", 5.0),
            dedup_glyph("C", 10.0),
        ];
        assert_eq!(dedup_texts(&mut v), vec!["A", "B", "C"]);
    }

    // -----------------------------------------------------------------------
    // 重複グリフ除去 parts 側（dedup_overlaid_parts）
    // -----------------------------------------------------------------------

    /// 除去テスト用の基準 parts グリフ
    fn dedup_part(ch: char, x: f64) -> GlyphPart {
        GlyphPart {
            ch,
            left: x,
            right: x + 5.0,
            top: 10.0,
            bottom: 20.0,
            font_size: Some(10.0),
            upright: true,
            rot: 0,
        }
    }

    /// 完全一致2個は先頭のみ残り順序が保たれる
    #[test]
    fn dedup_parts_exact_pair_keeps_first() {
        let mut v = vec![
            dedup_part('A', 0.0),
            dedup_part('B', 5.0),
            dedup_part('A', 0.0),
        ];
        dedup_overlaid_parts(&mut v);
        let chs: Vec<char> = v.iter().map(|g| g.ch).collect();
        assert_eq!(chs, vec!['A', 'B']);
    }

    /// キーのいずれか1箇所だけが違う組は両方残る
    #[test]
    fn dedup_parts_single_field_difference_keeps_both() {
        let variants: Vec<(&str, Box<dyn Fn(&mut GlyphPart)>)> = vec![
            ("ch", Box::new(|g| g.ch = 'B')),
            ("left", Box::new(|g| g.left += 0.5)),
            ("right", Box::new(|g| g.right += 0.5)),
            ("top", Box::new(|g| g.top += 0.5)),
            ("bottom", Box::new(|g| g.bottom += 0.5)),
            ("font_size", Box::new(|g| g.font_size = Some(11.0))),
            ("font_size_none", Box::new(|g| g.font_size = None)),
            ("upright", Box::new(|g| g.upright = false)),
            ("rot", Box::new(|g| g.rot = 180)),
        ];
        for (name, mutate) in variants {
            let mut b = dedup_part('A', 0.0);
            mutate(&mut b);
            let mut v = vec![dedup_part('A', 0.0), b];
            dedup_overlaid_parts(&mut v);
            assert_eq!(v.len(), 2, "field {name}");
        }
    }

    /// 非有限値を含むグリフは残る
    #[test]
    fn dedup_parts_keeps_nonfinite() {
        let muts: Vec<Box<dyn Fn(&mut GlyphPart)>> = vec![
            Box::new(|g| g.top = f64::INFINITY),
            Box::new(|g| g.font_size = Some(f64::NAN)),
        ];
        for (i, m) in muts.into_iter().enumerate() {
            let mut a = dedup_part('A', 0.0);
            m(&mut a);
            let mut b = dedup_part('A', 0.0);
            m(&mut b);
            let mut v = vec![a, b];
            dedup_overlaid_parts(&mut v);
            assert_eq!(v.len(), 2, "case {i}");
        }
    }

    /// +0.0 と -0.0 は同一視する
    #[test]
    fn dedup_parts_zero_sign_normalized() {
        let mut a = dedup_part('A', 0.0);
        a.left = 0.0;
        let mut b = dedup_part('A', 0.0);
        b.left = -0.0;
        let mut v = vec![a, b];
        dedup_overlaid_parts(&mut v);
        assert_eq!(v.len(), 1);
    }

    /// 同一テキストの二重描画が合成空白の生成前に畳まれる（extract_parts 経由）
    #[test]
    fn extract_parts_dedup_double_draw() {
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (AB) Tj 1 0 0 1 0 50 Tm (AB) Tj 1 0 0 1 13 50 Tm (CD) Tj ET";
        let ExtractedParts { glyphs, .. } = extract_parts(
            contents,
            &Dict::new(),
            default_view(),
            ContentCtx {
                data: &[],
                xref: None,
            },
            0,
        );
        let chs: Vec<char> = glyphs.iter().map(|g| g.ch).collect();
        assert_eq!(chs, vec!['A', 'B', 'C', 'D', ' '], "got {chs:?}");
    }

    /// 縦書きフォントの二重描画も先頭の1回分だけ残る（extract_text 経由）
    #[test]
    fn extract_text_vertical_dedup_double_draw() {
        let contents =
            b"BT /F1 10 Tf 1 0 0 1 50 150 Tm <00410042> Tj 1 0 0 1 50 150 Tm <00410042> Tj ET";
        let res = vertical_resources(false);
        let (_, _, fonts, chars) = text_of_res(contents, &res, default_view());
        assert!(fonts[0].vertical);
        assert_eq!(chars.len(), 2, "chars={}", chars.len());
        assert!(chars.iter().all(|c| !c.synthetic));
    }

    /// 同一テキストの二重描画が合成空白の生成前に畳まれる（extract_text 経由）
    #[test]
    fn extract_text_dedup_double_draw() {
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (AB) Tj 1 0 0 1 0 50 Tm (AB) Tj 1 0 0 1 13 50 Tm (CD) Tj ET";
        let (_, _, _, chars) = text_of(contents, default_view());
        let texts: Vec<&str> = chars.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["A", "B", " ", "C", "D"], "got {texts:?}");
        assert!(chars[2].synthetic);
    }

    /// 深くネストした配列リテラルでスタックオーバーフローしない
    #[test]
    fn parse_array_deep_nest_does_not_overflow() {
        let n = MAX_STREAM_PARSE_DEPTH + 100;
        let mut src = Vec::with_capacity(n * 2 + 1);
        for _ in 0..n {
            src.push(b'[');
        }
        for _ in 0..n {
            src.push(b']');
        }
        let mut lexer = Lexer::new(&src);
        assert!(matches!(lexer.next_token(), Token::ArrayStart));
        let obj = parse_array(&mut lexer, 0);
        assert!(matches!(obj, Object::Array(_)));
    }

    /// 深くネストした辞書リテラルでスタックオーバーフローしない
    #[test]
    fn parse_dict_deep_nest_does_not_overflow() {
        let n = MAX_STREAM_PARSE_DEPTH + 100;
        let mut src = Vec::with_capacity(n * 6 + 2);
        for _ in 0..n {
            src.extend_from_slice(b"<</k ");
        }
        src.extend_from_slice(b"null ");
        for _ in 0..n {
            src.extend_from_slice(b">>");
        }
        let mut lexer = Lexer::new(&src);
        assert!(matches!(lexer.next_token(), Token::DictStart));
        let obj = parse_dict(&mut lexer, 0);
        assert!(matches!(obj, Object::Dict(_)));
    }

    /// 上限ちょうどで内側が Null 化する
    #[test]
    fn parse_array_at_depth_limit_returns_null() {
        let obj = parse_array(&mut Lexer::new(b"[1 2 3]"), MAX_STREAM_PARSE_DEPTH);
        assert!(matches!(obj, Object::Null));
    }

    /// gstack 上限を超える q と対の Q でスタックオーバーフローせず、後続の描画も走る
    #[test]
    fn gstack_overflow_survives_and_paints_after() {
        let n = MAX_GSTACK_DEPTH + 100;
        let mut src: Vec<u8> = Vec::with_capacity(n * 4 + 32);
        for _ in 0..n {
            src.extend_from_slice(b"q ");
        }
        for _ in 0..n {
            src.extend_from_slice(b"Q ");
        }
        src.extend_from_slice(b"10 20 50 50 re S");
        let edges = edges_of(&src, default_view());
        assert_eq!(edges.len(), 4);
    }

    /// 上限超過ネストの外側の save/restore が保存され、外側の Q で ctm が正しく戻る
    #[test]
    fn gstack_overflow_preserves_outer_save_on_restore() {
        let mut src: Vec<u8> = Vec::new();
        src.extend_from_slice(b"1 0 0 1 10 0 cm ");
        src.extend_from_slice(b"q ");
        src.extend_from_slice(b"1 0 0 1 500 0 cm ");
        let n = MAX_GSTACK_DEPTH + 1;
        for _ in 0..n {
            src.extend_from_slice(b"q ");
        }
        for _ in 0..n {
            src.extend_from_slice(b"Q ");
        }
        src.extend_from_slice(b"Q ");
        src.extend_from_slice(b"0 20 100 20 re S");
        let edges = edges_of(&src, default_view());
        assert_eq!(edges.len(), 4);
        // ctm = translate(10, 0) が復元されている
        // rect (x=0, y=20, w=100, h=20) → PDF y 下辺 20 上辺 40。top-down は view.top(200)-y
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            10.0,
            110.0,
            180.0,
            180.0
        ));
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            10.0,
            110.0,
            160.0,
            160.0
        ));
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            10.0,
            10.0,
            160.0,
            180.0
        ));
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            110.0,
            110.0,
            160.0,
            180.0
        ));
    }

    /// resources に無い Tf 名前を上限を超える数だけ与えても panic しない
    #[test]
    fn font_memo_overflow_survives_when_tf_name_not_in_resources() {
        let n = MAX_FONT_MEMO_ENTRIES + 50;
        let mut src: Vec<u8> = b"BT ".to_vec();
        for i in 0..n {
            src.extend_from_slice(format!("/F{i} 12 Tf ").as_bytes());
        }
        src.extend_from_slice(b"ET");
        let edges = edges_of(&src, default_view());
        assert!(edges.is_empty());
    }
}
