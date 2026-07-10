//! 座標付き文字の公開型

use serde::{Deserialize, Serialize};

/// 文書全体の座標付き文字
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDoc {
    pub pages: Vec<TextPage>,
}

/// 1ページ分の座標付き文字
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextPage {
    pub width: f64,
    pub height: f64,
    pub fonts: Vec<TextFont>,
    pub chars: Vec<TextChar>,
}

/// ページ内フォント表の1エントリ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextFont {
    pub name: String,
    pub ascent: f64,
    pub descent: f64,
    pub vertical: bool,
}

/// 1グリフ分の座標付き文字
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextChar {
    pub text: String,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    /// グリフ開始時 TRM の表示座標 [a,b,c,d,e,f]
    pub transform: [f64; 6],
    /// 符号付き前進ベクトル（表示座標）
    pub advance: [f64; 2],
    pub font: u32,
    pub font_size: f64,
    pub rot: i32,
    pub upright: bool,
    pub synthetic: bool,
}

/// pdf.js TextItem 相当のチャンク
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextItem {
    #[serde(rename = "str")]
    pub r#str: String,
    pub dir: String,
    pub transform: [f64; 6],
    pub width: f64,
    pub height: f64,
    pub font: u32,
    pub has_eol: bool,
}

// pdf.js evaluator の閾値（fontSize 比）
const TRACKING_SPACE_FACTOR: f64 = 0.102;
const NOT_A_SPACE_FACTOR: f64 = 0.03;
const NEGATIVE_SPACE_FACTOR: f64 = -0.2;
const SPACE_IN_FLOW_MIN_FACTOR: f64 = 0.102;
const SPACE_IN_FLOW_MAX_FACTOR: f64 = 0.6;
const VERTICAL_SHIFT_RATIO: f64 = 0.25;

/// TextPage から pdf.js 相当の TextItem 列を構築する
pub fn build_text_items(page: &TextPage) -> Vec<TextItem> {
    let mut items: Vec<TextItem> = Vec::new();
    let mut chunk: Option<ChunkState> = None;
    // 直前2文字（空白合成判定用）。初期値は空白
    let mut last_chars = [' ', ' '];
    let mut last_pos: usize = 0;
    // 位置比較用の直前可視グリフ終端。flush 後も維持する
    let mut prev_transform: Option<[f64; 6]> = None;
    let mut prev_vertical = false;
    let mut prev_font_size: f64 = 0.0;

    for ch in &page.chars {
        if ch.synthetic {
            continue;
        }
        if ch.text.is_empty() {
            continue;
        }

        let vertical = page
            .fonts
            .get(ch.font as usize)
            .map(|f| f.vertical)
            .unwrap_or(false);
        let is_ws = is_whitespace_str(&ch.text);

        // 実 whitespace は str に入れず位置だけ進める
        if is_ws {
            save_last_char(&mut last_chars, &mut last_pos, ' ');
            // prev_transform は更新しない（次の非空白とのギャップに空白幅を含める）
            continue;
        }

        // フォント・サイズ変化で分割
        if let Some(c) = chunk.as_ref() {
            if c.font != ch.font || (c.font_size - ch.font_size).abs() > 1e-9 || c.vertical != vertical
            {
                flush_chunk(&mut items, &mut chunk);
                reset_last_chars(&mut last_chars, &mut last_pos);
            }
        }

        // 直前グリフとの位置関係
        if let Some(prev_xf) = prev_transform {
            let (mut pos_x, mut pos_y) = (ch.transform[4], ch.transform[5]);
            let (mut last_x, mut last_y) = (prev_xf[4], prev_xf[5]);

            if (last_x - pos_x).abs() > 1e-12 || (last_y - pos_y).abs() > 1e-12 {
                derotate_positions(
                    &ch.transform,
                    &prev_xf,
                    &mut pos_x,
                    &mut pos_y,
                    &mut last_x,
                    &mut last_y,
                );

                let font_size = if let Some(c) = chunk.as_ref() {
                    c.font_size
                } else {
                    prev_font_size.max(ch.font_size)
                };
                let thresholds = Thresholds::from_font_size(font_size);

                if vertical || prev_vertical {
                    // 表示座標は y 下向き。pdf.js（y 上向き）の last-pos と符号を揃える
                    let advance_y = pos_y - last_y;
                    let advance_x = pos_x - last_x;
                    let height_ref = chunk
                        .as_ref()
                        .map(|c| {
                            if c.signed_dim != 0.0 {
                                c.signed_dim
                            } else {
                                c.cross_dim
                            }
                        })
                        .unwrap_or(0.0);
                    let width_ref = chunk
                        .as_ref()
                        .map(|c| c.cross_dim)
                        .unwrap_or_else(|| hypot2(ch.transform[0], ch.transform[1]));
                    let text_orientation = sign_nonzero(height_ref);

                    if advance_y < text_orientation * thresholds.negative_space_max {
                        if advance_x.abs() > 0.5 * width_ref {
                            append_eol(&mut items, &mut chunk, &ch.transform, ch.font, &mut last_chars, &mut last_pos);
                        } else {
                            reset_last_chars(&mut last_chars, &mut last_pos);
                            flush_chunk(&mut items, &mut chunk);
                        }
                    } else if advance_x.abs() > width_ref {
                        append_eol(&mut items, &mut chunk, &ch.transform, ch.font, &mut last_chars, &mut last_pos);
                    } else {
                        if advance_y <= text_orientation * thresholds.not_a_space {
                            reset_last_chars(&mut last_chars, &mut last_pos);
                        }
                        if advance_y <= text_orientation * thresholds.tracking_space_min {
                            if should_add_whitespace(&last_chars, last_pos) {
                                reset_last_chars(&mut last_chars, &mut last_pos);
                                flush_chunk(&mut items, &mut chunk);
                                push_whitespace(
                                    &mut items,
                                    0.0,
                                    advance_y.abs(),
                                    prev_xf,
                                    ch.font,
                                );
                            } else if let Some(c) = chunk.as_mut() {
                                c.signed_dim += advance_y;
                            }
                        } else if !add_fake_spaces(
                            &mut items,
                            &mut chunk,
                            advance_y,
                            prev_xf,
                            text_orientation,
                            &thresholds,
                            true,
                            ch.font,
                            &mut last_chars,
                            &mut last_pos,
                        ) {
                            if chunk.as_ref().map(|c| c.str.is_empty()).unwrap_or(true) {
                                reset_last_chars(&mut last_chars, &mut last_pos);
                                push_whitespace(
                                    &mut items,
                                    0.0,
                                    advance_y.abs(),
                                    prev_xf,
                                    ch.font,
                                );
                            } else if let Some(c) = chunk.as_mut() {
                                c.signed_dim += advance_y;
                            }
                        }

                        let cross = chunk.as_ref().map(|c| c.cross_dim).unwrap_or(width_ref);
                        if advance_x.abs() > cross * VERTICAL_SHIFT_RATIO {
                            flush_chunk(&mut items, &mut chunk);
                        }
                    }
                } else {
                    // 横書き
                    let advance_x = pos_x - last_x;
                    let advance_y = pos_y - last_y;
                    let width_ref = chunk
                        .as_ref()
                        .map(|c| {
                            if c.signed_dim != 0.0 {
                                c.signed_dim
                            } else {
                                c.cross_dim
                            }
                        })
                        .unwrap_or(0.0);
                    let height_ref = chunk
                        .as_ref()
                        .map(|c| c.cross_dim)
                        .unwrap_or_else(|| hypot2(ch.transform[2], ch.transform[3]));
                    let text_orientation = sign_nonzero(width_ref);

                    if advance_x < text_orientation * thresholds.negative_space_max {
                        if advance_y.abs() > 0.5 * height_ref {
                            append_eol(&mut items, &mut chunk, &ch.transform, ch.font, &mut last_chars, &mut last_pos);
                        } else {
                            reset_last_chars(&mut last_chars, &mut last_pos);
                            flush_chunk(&mut items, &mut chunk);
                        }
                    } else if advance_y.abs() > height_ref {
                        append_eol(&mut items, &mut chunk, &ch.transform, ch.font, &mut last_chars, &mut last_pos);
                    } else {
                        if advance_x <= text_orientation * thresholds.not_a_space {
                            reset_last_chars(&mut last_chars, &mut last_pos);
                        }
                        if advance_x <= text_orientation * thresholds.tracking_space_min {
                            if should_add_whitespace(&last_chars, last_pos) {
                                reset_last_chars(&mut last_chars, &mut last_pos);
                                flush_chunk(&mut items, &mut chunk);
                                push_whitespace(
                                    &mut items,
                                    advance_x.abs(),
                                    0.0,
                                    prev_xf,
                                    ch.font,
                                );
                            } else if let Some(c) = chunk.as_mut() {
                                c.signed_dim += advance_x;
                            }
                        } else if !add_fake_spaces(
                            &mut items,
                            &mut chunk,
                            advance_x,
                            prev_xf,
                            text_orientation,
                            &thresholds,
                            false,
                            ch.font,
                            &mut last_chars,
                            &mut last_pos,
                        ) {
                            if chunk.as_ref().map(|c| c.str.is_empty()).unwrap_or(true) {
                                reset_last_chars(&mut last_chars, &mut last_pos);
                                push_whitespace(
                                    &mut items,
                                    advance_x.abs(),
                                    0.0,
                                    prev_xf,
                                    ch.font,
                                );
                            } else if let Some(c) = chunk.as_mut() {
                                c.signed_dim += advance_x;
                            }
                        }

                        let cross = chunk.as_ref().map(|c| c.cross_dim).unwrap_or(height_ref);
                        if advance_y.abs() > cross * VERTICAL_SHIFT_RATIO {
                            flush_chunk(&mut items, &mut chunk);
                        }
                    }
                }
            }
        }

        // チャンクへグリフを追加
        let c = chunk.get_or_insert_with(|| ChunkState::new(ch, vertical));
        if save_last_char_non_ws(&mut last_chars, &mut last_pos, &ch.text) {
            c.str.push(' ');
        }
        c.str.push_str(&ch.text);

        // pdf.js と同じく TJ 数値を除いた字送り幅を累積する
        let glyph_advance = glyph_advance(ch, vertical);
        let adv = progress_component(glyph_advance, vertical, &ch.transform);
        if vertical {
            c.signed_dim += adv.abs();
        } else {
            c.signed_dim += adv;
        }

        // グリフ幅だけ進んだ位置を prev にする
        prev_transform = Some([
            ch.transform[0],
            ch.transform[1],
            ch.transform[2],
            ch.transform[3],
            ch.transform[4] + glyph_advance[0],
            ch.transform[5] + glyph_advance[1],
        ]);
        prev_vertical = vertical;
        prev_font_size = ch.font_size;
    }

    flush_chunk(&mut items, &mut chunk);
    items
}

struct Thresholds {
    tracking_space_min: f64,
    not_a_space: f64,
    negative_space_max: f64,
    space_in_flow_min: f64,
    space_in_flow_max: f64,
}

impl Thresholds {
    fn from_font_size(font_size: f64) -> Self {
        Self {
            tracking_space_min: font_size * TRACKING_SPACE_FACTOR,
            not_a_space: font_size * NOT_A_SPACE_FACTOR,
            negative_space_max: font_size * NEGATIVE_SPACE_FACTOR,
            space_in_flow_min: font_size * SPACE_IN_FLOW_MIN_FACTOR,
            space_in_flow_max: font_size * SPACE_IN_FLOW_MAX_FACTOR,
        }
    }
}

struct ChunkState {
    str: String,
    transform: [f64; 6],
    /// 横書きは幅の符号付き累積、縦書きは高さの累積
    signed_dim: f64,
    /// 横書きは高さ、縦書きは幅
    cross_dim: f64,
    font: u32,
    font_size: f64,
    vertical: bool,
    has_eol: bool,
}

impl ChunkState {
    fn new(ch: &TextChar, vertical: bool) -> Self {
        let (signed_dim, cross_dim) = if vertical {
            (0.0, hypot2(ch.transform[0], ch.transform[1]))
        } else {
            (0.0, hypot2(ch.transform[2], ch.transform[3]))
        };
        Self {
            str: String::new(),
            transform: ch.transform,
            signed_dim,
            cross_dim,
            font: ch.font,
            font_size: ch.font_size,
            vertical,
            has_eol: false,
        }
    }

    fn into_item(self) -> TextItem {
        let (width, height) = if self.vertical {
            (self.cross_dim.abs(), self.signed_dim.abs())
        } else {
            (self.signed_dim.abs(), self.cross_dim.abs())
        };
        let dir = if self.vertical {
            "ttb".to_string()
        } else {
            "ltr".to_string()
        };
        TextItem {
            r#str: self.str,
            dir,
            transform: self.transform,
            width,
            height,
            font: self.font,
            has_eol: self.has_eol,
        }
    }
}

fn flush_chunk(items: &mut Vec<TextItem>, chunk: &mut Option<ChunkState>) {
    if let Some(c) = chunk.take() {
        if !c.str.is_empty() || c.has_eol {
            items.push(c.into_item());
        }
    }
}

fn append_eol(
    items: &mut Vec<TextItem>,
    chunk: &mut Option<ChunkState>,
    transform: &[f64; 6],
    font: u32,
    last_chars: &mut [char; 2],
    last_pos: &mut usize,
) {
    reset_last_chars(last_chars, last_pos);
    if let Some(c) = chunk.as_mut() {
        c.has_eol = true;
        flush_chunk(items, chunk);
    } else {
        items.push(TextItem {
            r#str: String::new(),
            dir: "ltr".to_string(),
            transform: *transform,
            width: 0.0,
            height: 0.0,
            font,
            has_eol: true,
        });
    }
}

fn push_whitespace(
    items: &mut Vec<TextItem>,
    width: f64,
    height: f64,
    transform: [f64; 6],
    font: u32,
) {
    items.push(TextItem {
        r#str: " ".to_string(),
        dir: "ltr".to_string(),
        transform,
        width,
        height,
        font,
        has_eol: false,
    });
}

/// ギャップが in-flow なら str に空白を足して false。範囲外なら分割+独立スペースで true
fn add_fake_spaces(
    items: &mut Vec<TextItem>,
    chunk: &mut Option<ChunkState>,
    width: f64,
    transf: [f64; 6],
    text_orientation: f64,
    thresholds: &Thresholds,
    vertical: bool,
    font: u32,
    last_chars: &mut [char; 2],
    last_pos: &mut usize,
) -> bool {
    if text_orientation * thresholds.space_in_flow_min <= width
        && width <= text_orientation * thresholds.space_in_flow_max
    {
        if let Some(c) = chunk.as_mut() {
            if !c.str.is_empty() {
                reset_last_chars(last_chars, last_pos);
                c.str.push(' ');
            }
        }
        return false;
    }

    let font_name = chunk.as_ref().map(|c| c.font).unwrap_or(font);
    let (w, h) = if vertical {
        (0.0, width.abs())
    } else {
        (width.abs(), 0.0)
    };
    flush_chunk(items, chunk);
    reset_last_chars(last_chars, last_pos);
    push_whitespace(items, w, h, transf, font_name);
    true
}

fn save_last_char(last_chars: &mut [char; 2], last_pos: &mut usize, ch: char) {
    last_chars[*last_pos] = ch;
    *last_pos = (*last_pos + 1) % 2;
}

/// 非空白を記録。直前が「非空白+空白」なら true（空白挿入）
fn save_last_char_non_ws(last_chars: &mut [char; 2], last_pos: &mut usize, text: &str) -> bool {
    let mut insert = false;
    for ch in text.chars() {
        let glyph = if ch.is_whitespace() { ' ' } else { ch };
        let next_pos = (*last_pos + 1) % 2;
        let ret = last_chars[*last_pos] != ' ' && last_chars[next_pos] == ' ';
        last_chars[*last_pos] = glyph;
        *last_pos = next_pos;
        if ret {
            insert = true;
        }
    }
    insert
}

fn should_add_whitespace(last_chars: &[char; 2], last_pos: usize) -> bool {
    last_chars[last_pos] != ' ' && last_chars[(last_pos + 1) % 2] == ' '
}

fn reset_last_chars(last_chars: &mut [char; 2], last_pos: &mut usize) {
    last_chars[0] = ' ';
    last_chars[1] = ' ';
    *last_pos = 0;
}

fn is_whitespace_str(s: &str) -> bool {
    !s.is_empty() && s.chars().all(char::is_whitespace)
}

fn hypot2(a: f64, b: f64) -> f64 {
    (a * a + b * b).sqrt()
}

fn sign_nonzero(v: f64) -> f64 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        1.0
    }
}

/// 前進ベクトルの進行軸成分
fn progress_component(advance: [f64; 2], vertical: bool, transform: &[f64; 6]) -> f64 {
    if vertical {
        // 縦書き: 主に y。符号は advance のまま（累積時に abs）
        // transform の縦軸が支配的な場合に射影
        let vx = transform[2];
        let vy = transform[3];
        let len = hypot2(vx, vy);
        if len > 1e-12 {
            (advance[0] * vx + advance[1] * vy) / len
        } else {
            advance[1]
        }
    } else {
        let ux = transform[0];
        let uy = transform[1];
        let len = hypot2(ux, uy);
        if len > 1e-12 {
            (advance[0] * ux + advance[1] * uy) / len
        } else {
            advance[0]
        }
    }
}

/// bbox から TJ 数値を含まない表示座標の字送りを得る
fn glyph_advance(ch: &TextChar, vertical: bool) -> [f64; 2] {
    if vertical {
        return ch.advance;
    }

    let axis_len = hypot2(ch.transform[0], ch.transform[1]);
    if axis_len <= 1e-12 {
        return ch.advance;
    }
    let ux = ch.transform[0] / axis_len;
    let uy = ch.transform[1] / axis_len;

    // 軸外回転のbboxは外接矩形のため字送り幅を一意に戻せない
    if ux.abs() > 1e-9 && uy.abs() > 1e-9 {
        return ch.advance;
    }

    let origin = ch.transform[4] * ux + ch.transform[5] * uy;
    let end = [
        ch.left * ux + ch.top * uy,
        ch.left * ux + ch.bottom * uy,
        ch.right * ux + ch.top * uy,
        ch.right * ux + ch.bottom * uy,
    ]
    .into_iter()
    .fold(f64::NEG_INFINITY, f64::max);
    let length = (end - origin).max(0.0);
    [ux * length, uy * length]
}

/// pdf.js compareWithLastPosition の回転正規化
fn derotate_positions(
    current: &[f64; 6],
    prev: &[f64; 6],
    pos_x: &mut f64,
    pos_y: &mut f64,
    last_x: &mut f64,
    last_y: &mut f64,
) {
    let rotate = detect_rotate(current);
    match rotate {
        0 => {}
        90 => {
            let (px, py) = (*pos_y, *pos_x);
            let (lx, ly) = (*last_y, *last_x);
            *pos_x = px;
            *pos_y = py;
            *last_x = lx;
            *last_y = ly;
        }
        180 => {
            *pos_x = -*pos_x;
            *pos_y = -*pos_y;
            *last_x = -*last_x;
            *last_y = -*last_y;
        }
        270 => {
            let (px, py) = (-*pos_y, -*pos_x);
            let (lx, ly) = (-*last_y, -*last_x);
            *pos_x = px;
            *pos_y = py;
            *last_x = lx;
            *last_y = ly;
        }
        _ => {
            let (px, py) = apply_inverse_rotation(*pos_x, *pos_y, current);
            let (lx, ly) = apply_inverse_rotation(*last_x, *last_y, prev);
            *pos_x = px;
            *pos_y = py;
            *last_x = lx;
            *last_y = ly;
        }
    }
}

fn detect_rotate(m: &[f64; 6]) -> i32 {
    if m[0] != 0.0 && m[1] == 0.0 && m[2] == 0.0 {
        if m[0] > 0.0 {
            0
        } else {
            180
        }
    } else if m[1] != 0.0 && m[0] == 0.0 && m[3] == 0.0 {
        if m[1] > 0.0 {
            90
        } else {
            270
        }
    } else {
        -1
    }
}

fn apply_inverse_rotation(x: f64, y: f64, matrix: &[f64; 6]) -> (f64, f64) {
    let scale = hypot2(matrix[0], matrix[1]);
    if scale < 1e-12 {
        return (x, y);
    }
    (
        (matrix[0] * x + matrix[1] * y) / scale,
        (matrix[2] * x + matrix[3] * y) / scale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        synthetic: bool,
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
            synthetic,
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
        let transform = if vertical {
            // 縦書き: 横軸が文字幅、縦軸が進行
            [font_size, 0.0, 0.0, -font_size, x, y]
        } else {
            [font_size, 0.0, 0.0, -font_size, x, y]
        };
        TextChar {
            text: text.into(),
            left: x,
            right: x + if vertical { font_size } else { adv_x.abs().max(1.0) },
            top: y,
            bottom: y + if vertical {
                adv_y.abs().max(font_size)
            } else {
                font_size
            },
            transform,
            advance: [adv_x, adv_y],
            font: font_idx,
            font_size,
            rot: 0,
            upright: !vertical,
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

    fn page_v(chars: Vec<TextChar>) -> TextPage {
        TextPage {
            width: 600.0,
            height: 800.0,
            fonts: vec![font(true)],
            chars,
        }
    }

    #[test]
    fn gap_tracking_no_space() {
        // font_size=10, gap=0.5 → 0.05fs ≤ 0.102 → tracking、空白なし
        let fs = 10.0;
        let adv = 5.0;
        let gap = 0.5;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv + gap, 100.0, adv, 0.0, fs, false),
        ]);
        let items = build_text_items(&p);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].r#str, "AB");
        assert!(!items[0].r#str.contains(' '));
        // 幅 = A.adv + gap + B.adv
        assert!((items[0].width - (adv + gap + adv)).abs() < 1e-9);
    }

    #[test]
    fn gap_in_flow_space() {
        // gap=2.0 → 0.2fs ∈ (0.102, 0.6] → 合成スペース
        let fs = 10.0;
        let adv = 5.0;
        let gap = 2.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv + gap, 100.0, adv, 0.0, fs, false),
        ]);
        let items = build_text_items(&p);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].r#str, "A B");
        assert!((items[0].width - (adv + gap + adv)).abs() < 1e-9);
    }

    #[test]
    fn gap_split_with_independent_space() {
        // gap=7.0 → 0.7fs > 0.6 → 分割 + 独立スペース item
        let fs = 10.0;
        let adv = 5.0;
        let gap = 7.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv + gap, 100.0, adv, 0.0, fs, false),
        ]);
        let items = build_text_items(&p);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].r#str, "A");
        assert_eq!(items[1].r#str, " ");
        assert!((items[1].width - gap).abs() < 1e-9);
        assert_eq!(items[2].r#str, "B");
    }

    #[test]
    fn tj_column_jump_splits_with_independent_space() {
        let fs = 10.0;
        let glyph_width = 5.0;
        let tj_jump = 7.0;
        let mut first = ch(
            "A",
            0.0,
            100.0,
            glyph_width + tj_jump,
            0.0,
            fs,
            false,
        );
        // bbox は字送り幅まで、advance は TJ 数値による移動も含む
        first.right = glyph_width;
        let p = page(vec![
            first,
            ch(
                "B",
                glyph_width + tj_jump,
                100.0,
                glyph_width,
                0.0,
                fs,
                false,
            ),
        ]);

        let items = build_text_items(&p);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].r#str, "A");
        assert!((items[0].width - glyph_width).abs() < 1e-9);
        assert_eq!(items[1].r#str, " ");
        assert!((items[1].width - tj_jump).abs() < 1e-9);
        assert_eq!(items[2].r#str, "B");
    }

    #[test]
    fn gap_negative_reverse_split() {
        // gap=-3.0 → -0.3fs < -0.2 → 逆行分割
        let fs = 10.0;
        let adv = 5.0;
        let gap = -3.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv + gap, 100.0, adv, 0.0, fs, false),
        ]);
        let items = build_text_items(&p);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].r#str, "A");
        assert_eq!(items[1].r#str, "B");
        assert!(!items[0].has_eol);
    }

    #[test]
    fn orthogonal_shift_25_percent_splits() {
        // height=10, dy=3 > 2.5 → 分割、EOL なし
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv, 103.0, adv, 0.0, fs, false),
        ]);
        let items = build_text_items(&p);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].r#str, "A");
        assert_eq!(items[1].r#str, "B");
        assert!(!items[0].has_eol);
    }

    #[test]
    fn orthogonal_shift_100_percent_has_eol() {
        // height=10, dy=11 > 10 → EOL
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv, 111.0, adv, 0.0, fs, false),
        ]);
        let items = build_text_items(&p);
        assert!(items.len() >= 2);
        assert_eq!(items[0].r#str, "A");
        assert!(items[0].has_eol);
        assert_eq!(items.last().unwrap().r#str, "B");
    }

    #[test]
    fn font_change_splits() {
        let fs = 10.0;
        let adv = 5.0;
        let mut p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv, 100.0, adv, 0.0, fs, false),
        ]);
        p.fonts.push(font(false));
        p.chars[1].font = 1;
        let items = build_text_items(&p);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].r#str, "A");
        assert_eq!(items[0].font, 0);
        assert_eq!(items[1].r#str, "B");
        assert_eq!(items[1].font, 1);
    }

    #[test]
    fn font_size_change_splits() {
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, 10.0, false),
            ch("B", adv, 100.0, adv, 0.0, 14.0, false),
        ]);
        let items = build_text_items(&p);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].r#str, "A");
        assert_eq!(items[1].r#str, "B");
    }

    #[test]
    fn vertical_chunk_dims_and_dir() {
        let fs = 10.0;
        // 縦書き: y 方向に並ぶ
        let p = page_v(vec![
            ch_at("あ", 50.0, 100.0, 0.0, 10.0, fs, 0, true),
            ch_at("い", 50.0, 110.0, 0.0, 10.0, fs, 0, true),
        ]);
        let items = build_text_items(&p);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].r#str, "あい");
        assert_eq!(items[0].dir, "ttb");
        // height = 縦前進の累積
        assert!((items[0].height - 20.0).abs() < 1e-6, "h={}", items[0].height);
        // width = TRM 横軸長
        assert!((items[0].width - fs).abs() < 1e-6, "w={}", items[0].width);
    }

    #[test]
    fn skips_synthetic_whitespace() {
        let fs = 10.0;
        let adv = 5.0;
        // A + synthetic space + B（位置は連続）
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch(" ", adv, 100.0, 3.0, 0.0, fs, true),
            ch("B", adv, 100.0, adv, 0.0, fs, false),
        ]);
        let items = build_text_items(&p);
        // synthetic を飛ばすと A と B が同位置で重なる扱い → 同一チャンク "AB"
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].r#str, "AB");
    }

    #[test]
    fn real_whitespace_replaced_by_u0020() {
        let fs = 10.0;
        let adv = 5.0;
        // 実タブグリフを挟む。位置ギャップが in-flow なら U+0020 になる
        let gap = 2.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("\t", adv, 100.0, gap, 0.0, fs, false),
            ch("B", adv + gap, 100.0, adv, 0.0, fs, false),
        ]);
        let items = build_text_items(&p);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].r#str, "A B");
        assert!(!items[0].r#str.contains('\t'));
    }

    #[test]
    fn continuous_no_gap() {
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("H", 0.0, 100.0, adv, 0.0, fs, false),
            ch("i", adv, 100.0, adv, 0.0, fs, false),
        ]);
        let items = build_text_items(&p);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].r#str, "Hi");
        assert_eq!(items[0].dir, "ltr");
        assert!((items[0].width - 10.0).abs() < 1e-9);
        assert!((items[0].height - fs).abs() < 1e-9);
        assert!(!items[0].has_eol);
    }
}
