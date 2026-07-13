//! 座標付き文字の公開型

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bidi::apply_bidi;

/// 文書全体の座標付き文字
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDoc {
    pub pages: Vec<TextPage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
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
    /// 太字属性（フォント資源から確定）
    #[serde(default)]
    pub bold: bool,
    /// イタリック属性（フォント資源から確定）
    #[serde(default)]
    pub italic: bool,
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
    /// 公称字送り長（Tc・Tw・TJ なしの進行軸方向長さ）。未設定時は bbox から復元
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glyph_width: Option<f64>,
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

/// 行ビュー
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextLine {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub dir: String,
    pub rot: i32,
    pub words: Vec<TextWord>,
    pub chars: Vec<u32>,
}

/// 単語ビュー
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextWord {
    pub text: String,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub chars: Vec<u32>,
}

// pdf.js evaluator の閾値（fontSize 比）
const TRACKING_SPACE_FACTOR: f64 = 0.102;
const NOT_A_SPACE_FACTOR: f64 = 0.03;
const NEGATIVE_SPACE_FACTOR: f64 = -0.2;
const SPACE_IN_FLOW_MIN_FACTOR: f64 = 0.102;
const SPACE_IN_FLOW_MAX_FACTOR: f64 = 0.6;
const VERTICAL_SHIFT_RATIO: f64 = 0.25;
/// 行クラスタのベースライン隣接差（font_size 比）
const LINE_CLUSTER_FACTOR: f64 = 0.6;
/// 行クラスタのベースライン全幅上限（font_size 比）
const LINE_BAND_SPAN_FACTOR: f64 = 0.7;
/// 進行軸の行分割空隙（font_size 比）
const LINE_PROGRESS_GAP_FACTOR: f64 = 2.0;

/// TextPage から pdf.js 相当の TextItem 列を構築する
pub fn build_text_items(page: &TextPage) -> Vec<TextItem> {
    build_text_items_with(page, false)
}

/// TextPage から TextItem 列を構築する（`bidi` で RTL 並べ替え）
pub fn build_text_items_with(page: &TextPage, bidi: bool) -> Vec<TextItem> {
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
            if c.font != ch.font
                || (c.font_size - ch.font_size).abs() > 1e-9
                || c.vertical != vertical
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
                            append_eol(
                                &mut items,
                                &mut chunk,
                                &ch.transform,
                                ch.font,
                                &mut last_chars,
                                &mut last_pos,
                            );
                        } else {
                            reset_last_chars(&mut last_chars, &mut last_pos);
                            flush_chunk(&mut items, &mut chunk);
                        }
                    } else if advance_x.abs() > width_ref {
                        append_eol(
                            &mut items,
                            &mut chunk,
                            &ch.transform,
                            ch.font,
                            &mut last_chars,
                            &mut last_pos,
                        );
                    } else {
                        if advance_y <= text_orientation * thresholds.not_a_space {
                            reset_last_chars(&mut last_chars, &mut last_pos);
                        }
                        if advance_y <= text_orientation * thresholds.tracking_space_min {
                            if should_add_whitespace(&last_chars, last_pos) {
                                reset_last_chars(&mut last_chars, &mut last_pos);
                                flush_chunk(&mut items, &mut chunk);
                                push_whitespace(&mut items, 0.0, advance_y.abs(), prev_xf, ch.font);
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
                                push_whitespace(&mut items, 0.0, advance_y.abs(), prev_xf, ch.font);
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
                            append_eol(
                                &mut items,
                                &mut chunk,
                                &ch.transform,
                                ch.font,
                                &mut last_chars,
                                &mut last_pos,
                            );
                        } else {
                            reset_last_chars(&mut last_chars, &mut last_pos);
                            flush_chunk(&mut items, &mut chunk);
                        }
                    } else if advance_y.abs() > height_ref {
                        append_eol(
                            &mut items,
                            &mut chunk,
                            &ch.transform,
                            ch.font,
                            &mut last_chars,
                            &mut last_pos,
                        );
                    } else {
                        if advance_x <= text_orientation * thresholds.not_a_space {
                            reset_last_chars(&mut last_chars, &mut last_pos);
                        }
                        if advance_x <= text_orientation * thresholds.tracking_space_min {
                            if should_add_whitespace(&last_chars, last_pos) {
                                reset_last_chars(&mut last_chars, &mut last_pos);
                                flush_chunk(&mut items, &mut chunk);
                                push_whitespace(&mut items, advance_x.abs(), 0.0, prev_xf, ch.font);
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
                                push_whitespace(&mut items, advance_x.abs(), 0.0, prev_xf, ch.font);
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
    if bidi {
        for item in &mut items {
            if item.dir == "ttb" {
                continue;
            }
            let r = apply_bidi(&item.r#str, false);
            item.r#str = r.str;
            item.dir = r.dir;
        }
    }
    items
}

/// TextPage から行・単語ビューを構築する
pub fn build_text_lines(page: &TextPage) -> Vec<TextLine> {
    build_text_lines_with(page, false)
}

/// TextPage から行・単語ビューを構築する（`bidi` で RTL 並べ替え）
pub fn build_text_lines_with(page: &TextPage, bidi: bool) -> Vec<TextLine> {
    let mut group_order: Vec<(bool, i32)> = Vec::new();
    let mut group_indices: Vec<Vec<usize>> = Vec::new();
    // 挿入順は group_order。位置引きだけ HashMap
    let mut group_pos: HashMap<(bool, i32), usize> = HashMap::new();

    for (i, ch) in page.chars.iter().enumerate() {
        if ch.synthetic || ch.text.is_empty() {
            continue;
        }
        let vertical = page
            .fonts
            .get(ch.font as usize)
            .map(|f| f.vertical)
            .unwrap_or(false);
        let key = (vertical, ch.rot);
        if let Some(&pos) = group_pos.get(&key) {
            group_indices[pos].push(i);
        } else {
            let pos = group_order.len();
            group_pos.insert(key, pos);
            group_order.push(key);
            group_indices.push(vec![i]);
        }
    }

    let mut lines: Vec<TextLine> = Vec::new();
    for (key, indices) in group_order.into_iter().zip(group_indices) {
        let (vertical, rot) = key;
        lines.extend(lines_for_group(page, &indices, vertical, rot, bidi));
    }

    lines.sort_by_cached_key(|line| line.chars.iter().copied().min().unwrap_or(u32::MAX));
    lines
}

/// 行クラスタ用のグリフ位置
#[derive(Clone)]
struct GlyphPos {
    idx: usize,
    baseline: f64,
    progress: f64,
    is_ws: bool,
}

/// 書字方向グループから行を構築する
fn lines_for_group(
    page: &TextPage,
    indices: &[usize],
    vertical: bool,
    rot: i32,
    bidi: bool,
) -> Vec<TextLine> {
    if indices.is_empty() {
        return Vec::new();
    }

    let mut glyphs: Vec<GlyphPos> = indices
        .iter()
        .map(|&idx| {
            let ch = &page.chars[idx];
            let (px, py) = derotate_xy(ch.transform[4], ch.transform[5], rot);
            let (baseline, progress) = if vertical { (px, py) } else { (py, px) };
            GlyphPos {
                idx,
                baseline,
                progress,
                is_ws: is_whitespace_str(&ch.text),
            }
        })
        .collect();

    let mut ns_sizes: Vec<f64> = indices
        .iter()
        .filter(|&&idx| !is_whitespace_str(&page.chars[idx].text))
        .map(|&idx| page.chars[idx].font_size)
        .filter(|fs| fs.is_finite())
        .collect();
    let median_fs = if ns_sizes.is_empty() {
        0.0
    } else {
        let n = ns_sizes.len();
        let cmp = |a: &f64, b: &f64| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal);
        if n % 2 == 1 {
            let mid = n / 2;
            ns_sizes.select_nth_unstable_by(mid, cmp);
            ns_sizes[mid]
        } else {
            let hi = n / 2;
            ns_sizes.select_nth_unstable_by(hi, cmp);
            let upper = ns_sizes[hi];
            let lower = ns_sizes[..hi]
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            (lower + upper) / 2.0
        }
    };
    let line_thresh = median_fs * LINE_CLUSTER_FACTOR;
    let band_thresh = median_fs * LINE_BAND_SPAN_FACTOR;
    let progress_gap_thresh = median_fs * LINE_PROGRESS_GAP_FACTOR;

    glyphs.sort_by(|a, b| {
        a.baseline
            .partial_cmp(&b.baseline)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.idx.cmp(&b.idx))
    });

    let dir = if vertical {
        "ttb".to_string()
    } else {
        "ltr".to_string()
    };

    let mut out = Vec::new();
    let mut cluster: Vec<GlyphPos> = Vec::new();
    let mut cluster_min_bl = f64::INFINITY;
    let mut cluster_max_bl = f64::NEG_INFINITY;
    for g in glyphs {
        if !cluster.is_empty() {
            let adj_cut = g.baseline - cluster.last().unwrap().baseline > line_thresh;
            let span_cut =
                cluster_max_bl.max(g.baseline) - cluster_min_bl.min(g.baseline) > band_thresh;
            if adj_cut || span_cut {
                emit_cluster_lines(
                    page,
                    &mut cluster,
                    vertical,
                    rot,
                    &dir,
                    progress_gap_thresh,
                    bidi,
                    &mut out,
                );
                cluster.clear();
                cluster_min_bl = f64::INFINITY;
                cluster_max_bl = f64::NEG_INFINITY;
            }
        }
        cluster_min_bl = cluster_min_bl.min(g.baseline);
        cluster_max_bl = cluster_max_bl.max(g.baseline);
        cluster.push(g);
    }
    emit_cluster_lines(
        page,
        &mut cluster,
        vertical,
        rot,
        &dir,
        progress_gap_thresh,
        bidi,
        &mut out,
    );
    out
}

/// 帯クラスタを進行軸空隙で分割して行を出す
fn emit_cluster_lines(
    page: &TextPage,
    cluster: &mut Vec<GlyphPos>,
    vertical: bool,
    rot: i32,
    dir: &str,
    progress_gap_thresh: f64,
    bidi: bool,
    out: &mut Vec<TextLine>,
) {
    if cluster.is_empty() {
        return;
    }
    for mut part in split_cluster_by_progress(page, cluster, vertical, rot, progress_gap_thresh) {
        if let Some(line) = line_from_cluster(page, &mut part, vertical, rot, dir, bidi) {
            out.push(line);
        }
    }
}

/// 進行軸の大きな bbox 間隙でクラスタを分割する
fn split_cluster_by_progress(
    page: &TextPage,
    cluster: &mut [GlyphPos],
    vertical: bool,
    rot: i32,
    progress_gap_thresh: f64,
) -> Vec<Vec<GlyphPos>> {
    cluster.sort_by(|a, b| {
        a.progress
            .partial_cmp(&b.progress)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.idx.cmp(&b.idx))
    });

    let mut parts: Vec<Vec<GlyphPos>> = Vec::new();
    let mut current: Vec<GlyphPos> = Vec::new();
    let mut pending_ws: Vec<GlyphPos> = Vec::new();
    let mut last_non_ws_idx: Option<usize> = None;

    for g in cluster.iter().cloned() {
        if g.is_ws {
            pending_ws.push(g);
            continue;
        }
        if let Some(prev_idx) = last_non_ws_idx {
            let (_prev_lo, prev_hi) = progress_bbox_extent(&page.chars[prev_idx], vertical, rot);
            let (next_lo, _next_hi) = progress_bbox_extent(&page.chars[g.idx], vertical, rot);
            let gap = next_lo - prev_hi;
            if gap > progress_gap_thresh {
                // 間の空白は前側へ
                current.extend(pending_ws.drain(..));
                parts.push(std::mem::take(&mut current));
            } else {
                current.extend(pending_ws.drain(..));
            }
        } else {
            // 先頭側に非空白が無い空白は後側へ
            current.extend(pending_ws.drain(..));
        }
        last_non_ws_idx = Some(g.idx);
        current.push(g);
    }
    current.extend(pending_ws);
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// 正立化後の進行軸での bbox 端
fn progress_bbox_extent(ch: &TextChar, vertical: bool, rot: i32) -> (f64, f64) {
    let corners = [
        (ch.left, ch.top),
        (ch.right, ch.top),
        (ch.left, ch.bottom),
        (ch.right, ch.bottom),
    ];
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for &(x, y) in &corners {
        let (dx, dy) = derotate_xy(x, y, rot);
        let p = if vertical { dy } else { dx };
        lo = lo.min(p);
        hi = hi.max(p);
    }
    (lo, hi)
}

/// クラスタから行ビューを組み立てる
fn line_from_cluster(
    page: &TextPage,
    cluster: &mut [GlyphPos],
    vertical: bool,
    rot: i32,
    dir: &str,
    bidi: bool,
) -> Option<TextLine> {
    if cluster.iter().all(|g| g.is_ws) {
        return None;
    }

    cluster.sort_by(|a, b| {
        a.progress
            .partial_cmp(&b.progress)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.idx.cmp(&b.idx))
    });

    let mut char_indices = Vec::with_capacity(cluster.len());
    let mut left = f64::INFINITY;
    let mut right = f64::NEG_INFINITY;
    let mut top = f64::INFINITY;
    let mut bottom = f64::NEG_INFINITY;
    let mut any_non_ws = false;
    for g in cluster.iter() {
        char_indices.push(g.idx as u32);
        if g.is_ws {
            continue;
        }
        let ch = &page.chars[g.idx];
        any_non_ws = true;
        left = left.min(ch.left);
        right = right.max(ch.right);
        top = top.min(ch.top);
        bottom = bottom.max(ch.bottom);
    }
    if !any_non_ws {
        return None;
    }

    let words = words_for_line(page, &char_indices, vertical, rot, bidi);
    let mut dir = dir.to_string();
    if bidi && !vertical {
        let visual: String = char_indices
            .iter()
            .map(|&i| page.chars[i as usize].text.as_str())
            .collect();
        dir = apply_bidi(&visual, false).dir;
    }
    Some(TextLine {
        left,
        right,
        top,
        bottom,
        dir,
        rot,
        words,
        chars: char_indices,
    })
}

/// 進行軸上の区間 [lo, hi]
fn progress_span(ch: &TextChar, vertical: bool) -> (f64, f64) {
    if vertical {
        let (a, b) = (ch.top, ch.bottom);
        (a.min(b), a.max(b))
    } else {
        let (a, b) = (ch.left, ch.right);
        (a.min(b), a.max(b))
    }
}

/// 進行軸ソートで空白の直前・直後の非空白が接触しているか
///
/// 語間に実ギャップがある通常スペースは false。接する数字の間に載った
/// レイアウト用スペースが true
fn whitespace_between_touching_solids(
    page: &TextPage,
    ws_idx: usize,
    order: &[u32],
    vertical: bool,
) -> bool {
    let mut solids: Vec<(f64, f64, f64)> = Vec::new();
    let mut ws_prog = None;
    for &i in order {
        let idx = i as usize;
        let ch = &page.chars[idx];
        let (s0, s1) = progress_span(ch, vertical);
        let key = s0;
        if idx == ws_idx {
            ws_prog = Some(key);
            continue;
        }
        if is_whitespace_str(&ch.text) {
            continue;
        }
        solids.push((key, s0, s1));
    }
    let Some(wp) = ws_prog else {
        return false;
    };
    solids.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut prev_end = None;
    let mut next_start = None;
    for &(key, s0, s1) in &solids {
        if key <= wp {
            prev_end = Some(s1);
        } else {
            next_start = Some(s0);
            break;
        }
    }
    match (prev_end, next_start) {
        (Some(l1), Some(r0)) => l1 >= r0 - 0.01,
        _ => false,
    }
}

/// 行内の単語を構築する
fn words_for_line(
    page: &TextPage,
    order: &[u32],
    vertical: bool,
    rot: i32,
    bidi: bool,
) -> Vec<TextWord> {
    let mut words: Vec<TextWord> = Vec::new();
    let mut cur: Vec<u32> = Vec::new();
    let mut prev_non_ws: Option<usize> = None;

    for &idx_u32 in order {
        let idx = idx_u32 as usize;
        let ch = &page.chars[idx];
        if is_whitespace_str(&ch.text) {
            // 接触する非空白の間に載った空白は語境界にしない
            if whitespace_between_touching_solids(page, idx, order, vertical) {
                continue;
            }
            if !cur.is_empty() {
                if let Some(w) = make_word(page, &cur, bidi) {
                    words.push(w);
                }
                cur.clear();
            }
            prev_non_ws = None;
            continue;
        }

        if let Some(prev_idx) = prev_non_ws {
            let gap = progress_gap(page, prev_idx, idx, vertical, rot);
            let fs = [page.chars[prev_idx].font_size, page.chars[idx].font_size]
                .into_iter()
                .filter(|fs| fs.is_finite())
                .reduce(f64::max);
            if fs.is_some_and(|fs| {
                gap > TRACKING_SPACE_FACTOR * fs || gap < NEGATIVE_SPACE_FACTOR * fs
            }) {
                if !cur.is_empty() {
                    if let Some(w) = make_word(page, &cur, bidi) {
                        words.push(w);
                    }
                    cur.clear();
                }
            }
        }

        cur.push(idx_u32);
        prev_non_ws = Some(idx);
    }

    if !cur.is_empty() {
        if let Some(w) = make_word(page, &cur, bidi) {
            words.push(w);
        }
    }
    words
}

/// 単語ビューの組み立て
fn make_word(page: &TextPage, chars: &[u32], bidi: bool) -> Option<TextWord> {
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
    if bidi {
        text = apply_bidi(&text, false).str;
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

/// 隣接グリフの進行軸ギャップ
fn progress_gap(
    page: &TextPage,
    prev_idx: usize,
    next_idx: usize,
    vertical: bool,
    rot: i32,
) -> f64 {
    let prev = &page.chars[prev_idx];
    let next = &page.chars[next_idx];
    let advance = glyph_advance(prev, vertical);
    let end_x = prev.transform[4] + advance[0];
    let end_y = prev.transform[5] + advance[1];
    let (end_dx, end_dy) = derotate_xy(end_x, end_y, rot);
    let (next_dx, next_dy) = derotate_xy(next.transform[4], next.transform[5], rot);
    if vertical {
        next_dy - end_dy
    } else {
        next_dx - end_dx
    }
}

/// 座標を -rot で正立へ揃える
pub(crate) fn derotate_xy(x: f64, y: f64, rot: i32) -> (f64, f64) {
    match rot {
        90 => (-y, x),
        180 => (-x, -y),
        270 => (y, -x),
        _ => (x, y),
    }
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

pub(crate) fn is_whitespace_str(s: &str) -> bool {
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

/// 字送り判定用の前進ベクトルを得る
///
/// 横書きは占有幅（bbox 射影。Tc 込み）と実前進（advance 射影。TJ 込み）の
/// 小さい方を使う。詰め配置（負の TJ）と字間トラッキング（Tc のみ）を両立する。
/// 縦書き・軸外は advance をそのまま使う
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
    let occ_len = (end - origin).max(0.0);
    // 上付きなど advance≈0 で幅だけあるグリフは占有幅を使う
    let adv_len = ch.advance[0] * ux + ch.advance[1] * uy;
    let length = if adv_len.is_finite() && adv_len > 1e-9 {
        occ_len.min(adv_len)
    } else {
        occ_len
    };
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
        if m[0] > 0.0 { 0 } else { 180 }
    } else if m[1] != 0.0 && m[0] == 0.0 && m[3] == 0.0 {
        if m[1] > 0.0 { 90 } else { 270 }
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
            bold: false,
            italic: false,
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
            glyph_width: None,
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
            transform,
            advance: [adv_x, adv_y],
            glyph_width: None,
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
        let mut first = ch("A", 0.0, 100.0, glyph_width + tj_jump, 0.0, fs, false);
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
        assert!(
            (items[0].height - 20.0).abs() < 1e-6,
            "h={}",
            items[0].height
        );
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

    // --- build_text_lines ---

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
            glyph_width: None,
            font: 0,
            font_size,
            rot,
            upright: rot == 0,
            synthetic: false,
        }
    }

    #[test]
    fn lines_horizontal_two_lines_and_words() {
        // 2行: "Hi" と "Yo"、同一行内は連続
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("H", 0.0, 100.0, adv, 0.0, fs, false),
            ch("i", adv, 100.0, adv, 0.0, fs, false),
            ch("Y", 0.0, 120.0, adv, 0.0, fs, false),
            ch("o", adv, 120.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].dir, "ltr");
        assert_eq!(lines[0].rot, 0);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "Hi");
        assert_eq!(lines[0].chars, vec![0, 1]);
        assert_eq!(lines[1].words[0].text, "Yo");
        assert_eq!(lines[1].chars, vec![2, 3]);
    }

    #[test]
    fn lines_word_gap_splits() {
        // gap=2.0 → 0.2fs > 0.102 → 語境界
        let fs = 10.0;
        let adv = 5.0;
        let gap = 2.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv + gap, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[0].text, "A");
        assert_eq!(lines[0].words[1].text, "B");
        assert_eq!(lines[0].chars, vec![0, 1]);
    }

    /// 詰め配置（bbox>advance）でも min により同一単語に保つ
    #[test]
    fn lines_min_width_keeps_tight_tc_pack() {
        let fs = 10.0;
        let pure = 5.0;
        let tc = 3.0;
        // 原点間隔 = pure（TJ で Tc を打ち消し）、bbox 幅 = pure+Tc
        let mut a = ch("A", 0.0, 100.0, pure + tc, 0.0, fs, false);
        a.right = pure + tc;
        a.advance = [pure, 0.0];
        let mut b = ch("B", pure, 100.0, pure + tc, 0.0, fs, false);
        b.right = pure + pure + tc;
        b.left = pure;
        b.advance = [pure, 0.0];
        let p = page(vec![a, b]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "AB");
        let items = build_text_items(&p);
        assert_eq!(items[0].r#str, "AB");
    }

    /// Tc トラッキング（bbox≈advance>pure）でも同一単語に保つ
    #[test]
    fn lines_min_width_keeps_tc_tracking() {
        let fs = 10.0;
        let pure = 5.0;
        let tc = 3.0;
        let step = pure + tc;
        let mut a = ch("A", 0.0, 100.0, step, 0.0, fs, false);
        a.right = step;
        a.advance = [step, 0.0];
        a.glyph_width = Some(pure);
        let mut b = ch("B", step, 100.0, step, 0.0, fs, false);
        b.left = step;
        b.right = step + step;
        b.advance = [step, 0.0];
        b.glyph_width = Some(pure);
        let p = page(vec![a, b]);
        let lines = build_text_lines(&p);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "AB");
    }

    /// advance≈0 の上付きは占有幅で次と結合する
    #[test]
    fn lines_zero_advance_superscript_stays_in_word() {
        let fs = 10.0;
        let mut three = ch("3", 0.0, 100.0, 5.0, 0.0, fs, false);
        three.right = 5.0;
        three.advance = [0.0, 0.0];
        let mut r = ch("r", 5.0, 100.0, 3.0, 0.0, 7.0, false);
        r.left = 5.0;
        r.right = 8.0;
        r.advance = [3.0, 0.0];
        let p = page(vec![three, r]);
        let lines = build_text_lines(&p);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "3r");
    }

    #[test]
    fn lines_tj_gap_splits_word() {
        // TJ を含む advance でも bbox 後の語間を検出
        let fs = 10.0;
        let glyph_width = 5.0;
        let gap = 2.0;
        let mut first = ch("A", 0.0, 100.0, glyph_width, 0.0, fs, false);
        first.advance[0] += gap;
        let p = page(vec![
            first,
            ch("B", glyph_width + gap, 100.0, glyph_width, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[0].text, "A");
        assert_eq!(lines[0].words[1].text, "B");
    }

    #[test]
    fn lines_real_whitespace_splits_word() {
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch(" ", adv, 100.0, 3.0, 0.0, fs, false),
            ch("B", adv + 3.0, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].chars, vec![0, 1, 2]);
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[0].text, "A");
        assert_eq!(lines[0].words[1].text, "B");
        // 空白は単語に含めない
        assert_eq!(lines[0].words[0].chars, vec![0]);
        assert_eq!(lines[0].words[1].chars, vec![2]);
    }

    /// 接する数字の間に載った空白は語境界にしない
    #[test]
    fn lines_overlapping_whitespace_does_not_split_word() {
        let fs = 10.0;
        // "1" "4" が接し、空白が間に載る（x 順は 1, space, 4）
        let mut one = ch("1", 0.0, 100.0, 5.0, 0.0, fs, false);
        one.right = 5.0;
        let mut sp = ch(" ", 1.0, 100.0, 2.0, 0.0, fs, false);
        sp.left = 1.0;
        sp.right = 3.0;
        let mut four = ch("4", 5.0, 100.0, 5.0, 0.0, fs, false);
        four.left = 5.0;
        four.right = 10.0;
        let p = page(vec![one, sp, four]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "14");
    }

    #[test]
    fn lines_vertical_ttb() {
        let fs = 10.0;
        let p = page_v(vec![
            ch_at("あ", 50.0, 100.0, 0.0, 10.0, fs, 0, true),
            ch_at("い", 50.0, 110.0, 0.0, 10.0, fs, 0, true),
            // 別列（ベースライン e が離れる）
            ch_at("う", 80.0, 100.0, 0.0, 10.0, fs, 0, true),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].dir, "ttb");
        assert_eq!(lines[0].words[0].text, "あい");
        assert_eq!(lines[1].words[0].text, "う");
    }

    #[test]
    fn lines_rotated_group() {
        // rot 90 の横書き3文字は表示 -y へ進む
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch_rot("A", 100.0, 60.0, adv, fs, 90),
            ch_rot("B", 100.0, 60.0 - adv, adv, fs, 90),
            ch_rot("C", 100.0, 60.0 - adv * 2.0, adv, fs, 90),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].dir, "ltr");
        assert_eq!(lines[0].rot, 90);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "ABC");
        assert_eq!(lines[0].words[0].chars, vec![0, 1, 2]);
        assert_eq!(lines[0].chars, vec![0, 1, 2]);
    }

    #[test]
    fn lines_mixed_horizontal_vertical() {
        let fs = 10.0;
        let adv = 5.0;
        let mut p = TextPage {
            width: 600.0,
            height: 800.0,
            fonts: vec![font(false), font(true)],
            chars: vec![
                ch("H", 0.0, 100.0, adv, 0.0, fs, false),
                ch("i", adv, 100.0, adv, 0.0, fs, false),
                ch_at("あ", 200.0, 50.0, 0.0, 10.0, fs, 1, true),
                ch_at("い", 200.0, 60.0, 0.0, 10.0, fs, 1, true),
            ],
        };
        p.chars[0].font = 0;
        p.chars[1].font = 0;
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        // ストリーム初出順
        assert_eq!(lines[0].dir, "ltr");
        assert_eq!(lines[0].words[0].text, "Hi");
        assert_eq!(lines[1].dir, "ttb");
        assert_eq!(lines[1].words[0].text, "あい");
    }

    #[test]
    fn lines_whitespace_only_line_excluded() {
        let fs = 10.0;
        let adv = 5.0;
        // y=100 に文字、y=130 に空白のみ（中央値 fs=10 → 閾値 6、差 30 で別行）
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch(" ", 0.0, 130.0, adv, 0.0, fs, false),
            ch("\t", adv, 130.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words[0].text, "A");
    }

    #[test]
    fn lines_skips_synthetic() {
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch(" ", adv, 100.0, 3.0, 0.0, fs, true),
            ch("B", adv, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        // 合成空白は無視し、同位置の A/B は同一単語
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "AB");
        assert_eq!(lines[0].chars, vec![0, 2]);
    }

    #[test]
    fn lines_negative_gap_splits_word() {
        // gap=-3 → -0.3fs < -0.2 → 逆行で語境界
        let fs = 10.0;
        let adv = 5.0;
        let gap = -3.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv + gap, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        // 進行軸順は A(x=0) → B(x=2)。逆行ギャップで語分割
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[0].text, "A");
        assert_eq!(lines[0].words[1].text, "B");
    }

    #[test]
    fn lines_follow_stream_order_not_coordinate_order() {
        // 座標で下の行が先ならストリーム順を保つ
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("L", 0.0, 120.0, adv, 0.0, fs, false),
            ch("o", adv, 120.0, adv, 0.0, fs, false),
            ch("w", adv * 2.0, 120.0, adv, 0.0, fs, false),
            ch("U", 0.0, 100.0, adv, 0.0, fs, false),
            ch("p", adv, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].words[0].text, "Low");
        assert_eq!(lines[0].chars, vec![0, 1, 2]);
        assert_eq!(lines[1].words[0].text, "Up");
        assert_eq!(lines[1].chars, vec![3, 4]);
    }

    #[test]
    fn lines_rotated_180_and_270_follow_progress_axis() {
        // rot 180 と rot 270 は各表示進行方向へ並ぶ
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch_rot("A", 100.0, 50.0, adv, fs, 180),
            ch_rot("B", 100.0 - adv, 50.0, adv, fs, 180),
            ch_rot("C", 100.0 - adv * 2.0, 50.0, adv, fs, 180),
            ch_rot("D", 200.0, 80.0, adv, fs, 270),
            ch_rot("E", 200.0, 80.0 + adv, adv, fs, 270),
            ch_rot("F", 200.0, 80.0 + adv * 2.0, adv, fs, 270),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].rot, 180);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "ABC");
        assert_eq!(lines[0].words[0].chars, vec![0, 1, 2]);
        assert_eq!(lines[0].chars, vec![0, 1, 2]);
        assert_eq!(lines[1].rot, 270);
        assert_eq!(lines[1].words.len(), 1);
        assert_eq!(lines[1].words[0].text, "DEF");
        assert_eq!(lines[1].words[0].chars, vec![3, 4, 5]);
        assert_eq!(lines[1].chars, vec![3, 4, 5]);
    }

    #[test]
    fn lines_baseline_difference_at_threshold_stays_together() {
        // ベースライン差が 0.6fs ちょうどなら同一行
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv, 100.0 + 0.6 * fs, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words[0].text, "AB");
        assert_eq!(lines[0].chars, vec![0, 1]);
    }

    #[test]
    fn lines_progress_gap_at_threshold_stays_in_word() {
        // 進行軸ギャップが 0.102fs ちょうどなら同一単語
        let fs = 10.0;
        let adv = 5.0;
        let gap = TRACKING_SPACE_FACTOR * fs;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv + gap, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 1);
        assert_eq!(lines[0].words[0].text, "AB");
    }

    #[test]
    fn lines_ignore_non_finite_font_size_for_clustering() {
        // NaN の font_size が混ざっても有限値で行を分ける
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("N", adv, 100.0, adv, 0.0, f64::NAN, false),
            ch("B", 0.0, 120.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].words[0].text, "AN");
        assert_eq!(lines[0].chars, vec![0, 1]);
        assert_eq!(lines[1].words[0].text, "B");
        assert_eq!(lines[1].chars, vec![2]);
    }

    #[test]
    fn lines_band_span_breaks_bridge_chain() {
        // Mid-height bridge keeps adj gaps under 0.6fs; span 0.7fs splits into multiple lines
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv, 105.0, adv, 0.0, fs, false),
            ch("C", adv * 2.0, 110.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert!(
            lines.len() >= 2,
            "expected span cut, got {} lines",
            lines.len()
        );
        let joined: String = lines
            .iter()
            .flat_map(|l| l.words.iter().map(|w| w.text.as_str()))
            .collect();
        assert_eq!(joined, "ABC");
        assert!(
            lines
                .iter()
                .all(|l| l.words.iter().map(|w| w.text.len()).sum::<usize>() < 3)
        );
    }

    #[test]
    fn lines_progress_gap_splits_same_baseline_columns() {
        // Two runs on the same baseline with a large progress-axis gutter
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("L", 0.0, 100.0, adv, 0.0, fs, false),
            ch("e", adv, 100.0, adv, 0.0, fs, false),
            ch("R", 100.0, 100.0, adv, 0.0, fs, false),
            ch("t", 100.0 + adv, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].words[0].text, "Le");
        assert_eq!(lines[0].chars, vec![0, 1]);
        assert_eq!(lines[1].words[0].text, "Rt");
        assert_eq!(lines[1].chars, vec![2, 3]);
    }

    #[test]
    fn lines_superscript_stays_in_band() {
        // Superscript within band width stays on the body line
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("2", adv, 97.0, adv, 0.0, fs * 0.6, false),
            ch("B", adv * 2.0, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].chars, vec![0, 1, 2]);
        assert_eq!(lines[0].words[0].text, "A2B");
    }

    #[test]
    fn lines_progress_gap_whitespace_attaches_to_previous() {
        // Space between a large progress gap goes to the left line
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch(" ", 40.0, 100.0, 5.0, 0.0, fs, false),
            ch("B", 100.0, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].chars, vec![0, 1]);
        assert_eq!(lines[0].words[0].text, "A");
        assert_eq!(lines[1].chars, vec![2]);
        assert_eq!(lines[1].words[0].text, "B");
    }

    #[test]
    fn lines_progress_gap_leading_whitespace_attaches_to_next() {
        // Leading space with no prior non-ws attaches to the following line
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch(" ", 0.0, 100.0, 5.0, 0.0, fs, false),
            ch("A", 10.0, 100.0, adv, 0.0, fs, false),
            ch("B", 100.0, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].chars, vec![0, 1]);
        assert_eq!(lines[0].words[0].text, "A");
        assert_eq!(lines[1].chars, vec![2]);
        assert_eq!(lines[1].words[0].text, "B");
    }

    #[test]
    fn lines_progress_gap_at_or_below_factor_stays_one_line() {
        // Gaps at or below 2.0× median (word/tab scale) do not split the line
        let fs = 10.0;
        let adv = 5.0;
        let gap = LINE_PROGRESS_GAP_FACTOR * fs;
        let p = page(vec![
            ch("A", 0.0, 100.0, adv, 0.0, fs, false),
            ch("B", adv + gap, 100.0, adv, 0.0, fs, false),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].chars, vec![0, 1]);
        // Word split still applies (gap >> 0.102fs)
        assert_eq!(lines[0].words.len(), 2);
        assert_eq!(lines[0].words[0].text, "A");
        assert_eq!(lines[0].words[1].text, "B");
    }

    #[test]
    fn text_font_serde_default_bold_italic_false() {
        let json = r#"{"name":"H","ascent":0.8,"descent":-0.2,"vertical":false}"#;
        let font: TextFont = serde_json::from_str(json).unwrap();
        assert!(!font.bold);
        assert!(!font.italic);
        assert_eq!(font.name, "H");
    }

    #[test]
    fn lines_vertical_progress_gap_splits() {
        // Vertical group: large y gutter on the same baseline splits lines
        let fs = 10.0;
        let p = page_v(vec![
            ch_at("あ", 50.0, 100.0, 0.0, 10.0, fs, 0, true),
            ch_at("い", 50.0, 110.0, 0.0, 10.0, fs, 0, true),
            ch_at("う", 50.0, 200.0, 0.0, 10.0, fs, 0, true),
            ch_at("え", 50.0, 210.0, 0.0, 10.0, fs, 0, true),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].dir, "ttb");
        assert_eq!(lines[0].words[0].text, "あい");
        assert_eq!(lines[1].words[0].text, "うえ");
    }

    #[test]
    fn lines_rotated_90_progress_gap_splits() {
        // rot 90: two runs separated along the derotated progress axis
        let fs = 10.0;
        let adv = 5.0;
        let p = page(vec![
            ch_rot("A", 100.0, 60.0, adv, fs, 90),
            ch_rot("B", 100.0, 60.0 - adv, adv, fs, 90),
            // large gap in -y / derotated progress
            ch_rot("C", 100.0, 0.0, adv, fs, 90),
            ch_rot("D", 100.0, 0.0 - adv, adv, fs, 90),
        ]);
        let lines = build_text_lines(&p);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].rot, 90);
        assert_eq!(lines[0].words[0].text, "AB");
        assert_eq!(lines[0].chars, vec![0, 1]);
        assert_eq!(lines[1].words[0].text, "CD");
        assert_eq!(lines[1].chars, vec![2, 3]);
    }
}
