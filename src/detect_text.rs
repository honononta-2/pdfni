//! 文字整列方式の表検出

use crate::model::{BBox, Cell, Table};
use std::collections::HashMap;

/// 検出単位の文字塊
pub struct Seg {
    pub text: String,
    pub bbox: BBox,
    /// リーダー runを境界として切り出されたセグか
    pub leader_adj: bool,
}

/// 検出パラメータ
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct TextSettings {
    pub align_tol: f64,
    pub min_table_segs: usize,
    pub col_gap: f64,
    pub min_rows: usize,
    pub min_cols: usize,
    pub min_fill: f64,
    /// 型の揃った列が無い表に要求する最小行数
    pub untyped_min_rows: usize,
    /// 文章らしいセルの許容割合
    pub max_prose_ratio: f64,
    /// 文章比率超過を整列で救済する場合の最小充填率
    pub prose_override_min_fill: f64,
    /// 同じく各列の左端整列率の下限
    pub prose_override_min_align: f64,
    /// 同じく最小行数
    pub prose_override_min_rows: usize,
}

impl Default for TextSettings {
    fn default() -> Self {
        Self {
            align_tol: 1.0,
            min_table_segs: 4,
            col_gap: 1.0,
            min_rows: 2,
            min_cols: 2,
            min_fill: 0.3,
            untyped_min_rows: 4,
            max_prose_ratio: 0.4,
            prose_override_min_fill: 0.9,
            prose_override_min_align: 0.95,
            prose_override_min_rows: 4,
        }
    }
}

/// 文章セル比率が許容を超えるか
///
/// リーダー runで区切られたセルは文章に数えない
fn prose_heavy(t: &Table, leader: &[Vec<bool>], s: &TextSettings) -> bool {
    let filled = t
        .data
        .iter()
        .flatten()
        .filter(|c| !c.text.trim().is_empty())
        .count();
    let prose = t
        .data
        .iter()
        .enumerate()
        .flat_map(|(r, row)| row.iter().enumerate().map(move |(c, cell)| (r, c, cell)))
        .filter(|&(r, c, cell)| is_prose_cell(&cell.text) && !leader[r][c])
        .count();
    (prose as f64) > s.max_prose_ratio * filled as f64
}

/// 文章らしいセルか
pub(crate) fn is_prose_cell(s: &str) -> bool {
    let t = s.trim();
    let n = t.chars().count();
    if n == 0 {
        return false;
    }
    n >= 20
        || t.contains('。')
        || t.contains('@')
        || t.ends_with(':')
        || t.ends_with('：')
        || t.contains('：')
        || t.contains(": ")
}

/// 文章比率が高くても表として受理する整列条件
///
/// ほぼ全セルが埋まり各列の左端が揃う候補は長文セルの表とみなす
fn aligned_full_grid(t: &Table, s: &TextSettings) -> bool {
    if t.n_rows < s.prose_override_min_rows {
        return false;
    }
    let filled = t
        .data
        .iter()
        .flatten()
        .filter(|c| !c.text.trim().is_empty())
        .count();
    if (filled as f64) < s.prose_override_min_fill * (t.n_rows * t.n_cols) as f64 {
        return false;
    }
    let mut aligns: Vec<f64> = Vec::new();
    for c in 0..t.n_cols {
        let x0s: Vec<f64> = t
            .data
            .iter()
            .filter_map(|r| r.get(c))
            .filter(|cell| !cell.text.trim().is_empty() && cell.bbox.width() > 0.0)
            .map(|cell| cell.bbox.x0)
            .collect();
        if x0s.len() < 2 {
            continue;
        }
        let med = median(x0s.clone());
        let hit = x0s.iter().filter(|x| (*x - med).abs() <= 2.0).count();
        aligns.push(hit as f64 / x0s.len() as f64);
    }
    !aligns.is_empty()
        && aligns.iter().sum::<f64>() / aligns.len() as f64 >= s.prose_override_min_align
}

/// 数値や日付とみなせるセルか
fn is_typed_cell(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.chars().any(|c| c.is_ascii_digit())
        && !t.chars().any(|c| c.is_ascii_alphabetic())
}

/// 型の揃った列を持つか
fn has_typed_column(t: &Table) -> bool {
    (0..t.n_cols).any(|c| {
        let texts: Vec<&str> = t
            .data
            .iter()
            .filter_map(|r| r.get(c))
            .map(|cell| cell.text.as_str())
            .filter(|s| !s.trim().is_empty())
            .collect();
        let typed = texts.iter().filter(|s| is_typed_cell(s)).count();
        typed >= 2 && (typed as f64) >= 0.6 * texts.len() as f64
    })
}

/// 各値が属する許容差クラスタのid
pub(crate) fn cluster_ids(vals: &[f64], tol: f64) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..vals.len()).collect();
    idx.sort_by(|&a, &b| vals[a].total_cmp(&vals[b]));
    let mut ids = vec![0usize; vals.len()];
    let mut cur = 0usize;
    let mut prev = f64::NEG_INFINITY;
    for (n, &i) in idx.iter().enumerate() {
        if n > 0 && vals[i] - prev > tol {
            cur += 1;
        }
        ids[i] = cur;
        prev = vals[i];
    }
    ids
}

/// 各値が属するクラスタの要素数
pub(crate) fn group_sizes(vals: &[f64], tol: f64) -> Vec<usize> {
    let ids = cluster_ids(vals, tol);
    let max_id = ids.iter().copied().max().unwrap_or(0);
    let mut counts = vec![0usize; max_id + 1];
    for &g in &ids {
        counts[g] += 1;
    }
    ids.iter().map(|&g| counts[g]).collect()
}

pub(crate) fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// 縦横どちらかでしか整列しないセグメントを反復して落とす
fn connected_set(segs: &[Seg], tol: f64) -> Vec<usize> {
    let n0 = segs.len();
    let mut active: Vec<usize> = (0..n0).collect();
    let mut lefts: Vec<f64> = Vec::with_capacity(n0);
    let mut rights: Vec<f64> = Vec::with_capacity(n0);
    let mut hmids: Vec<f64> = Vec::with_capacity(n0);
    let mut tops: Vec<f64> = Vec::with_capacity(n0);
    let mut bots: Vec<f64> = Vec::with_capacity(n0);
    let mut vmids: Vec<f64> = Vec::with_capacity(n0);
    let mut keep: Vec<usize> = Vec::with_capacity(n0);
    loop {
        let n = active.len();
        if n == 0 {
            break;
        }
        lefts.clear();
        rights.clear();
        hmids.clear();
        tops.clear();
        bots.clear();
        vmids.clear();
        for &i in &active {
            let b = &segs[i].bbox;
            lefts.push(b.x0);
            rights.push(b.x1);
            hmids.push(b.cx());
            tops.push(b.top);
            bots.push(b.bottom);
            vmids.push(b.cy());
        }

        let gl = group_sizes(&lefts, tol);
        let gr = group_sizes(&rights, tol);
        let gm = group_sizes(&hmids, tol);
        let gt = group_sizes(&tops, tol);
        let gb = group_sizes(&bots, tol);
        let gv = group_sizes(&vmids, tol);

        keep.clear();
        for k in 0..n {
            let col_count = gl[k].max(gr[k]).max(gm[k]);
            let row_count = gt[k].max(gb[k]).max(gv[k]);
            if col_count >= 2 && row_count >= 2 {
                keep.push(active[k]);
            }
        }
        if keep.len() == n {
            break;
        }
        std::mem::swap(&mut active, &mut keep);
    }
    active
}

/// 水平に重なるセグメントの x 区間をまとめて列帯にする
pub(crate) fn column_bands(segs: &[Seg], members: &[usize], gap: f64) -> Vec<(f64, f64)> {
    let mut iv: Vec<(f64, f64)> = members
        .iter()
        .map(|&i| (segs[i].bbox.x0, segs[i].bbox.x1))
        .collect();
    iv.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut bands: Vec<(f64, f64)> = Vec::new();
    for (lo, hi) in iv {
        match bands.last_mut() {
            Some(b) if lo <= b.1 + gap => {
                if hi > b.1 {
                    b.1 = hi;
                }
            }
            _ => bands.push((lo, hi)),
        }
    }
    bands
}

/// 基準行の塊から作る列帯
fn anchor_column_bands(
    segs: &[Seg],
    members: &[usize],
    row_ids: &[usize],
    n_rows: usize,
    s: &TextSettings,
) -> Vec<(f64, f64)> {
    let mut rows: Vec<Vec<usize>> = vec![Vec::new(); n_rows];
    for (k, &i) in members.iter().enumerate() {
        rows[row_ids[k]].push(i);
    }
    let mut counts: HashMap<usize, usize> = HashMap::new();
    for r in &rows {
        if !r.is_empty() {
            *counts.entry(r.len()).or_insert(0) += 1;
        }
    }
    fn mode_of(counts: &HashMap<usize, usize>) -> Option<usize> {
        counts.iter().max_by_key(|&(&k, &v)| (v, k)).map(|(&k, _)| k)
    }
    let mut ncols = match mode_of(&counts) {
        Some(n) => n,
        None => return column_bands(segs, members, s.col_gap),
    };
    if ncols == 1 {
        counts.remove(&1);
        ncols = match mode_of(&counts) {
            Some(n) => n,
            None => return column_bands(segs, members, s.col_gap),
        };
    }
    if ncols < 2 {
        return column_bands(segs, members, s.col_gap);
    }
    let anchors: Vec<usize> = rows
        .iter()
        .filter(|r| r.len() == ncols)
        .flatten()
        .copied()
        .collect();
    let mut bands = column_bands(segs, &anchors, s.col_gap);
    // 帯に重ならない塊からの列の補完
    let extras: Vec<usize> = members
        .iter()
        .copied()
        .filter(|&i| {
            let b = &segs[i].bbox;
            !bands.iter().any(|&(lo, hi)| b.x0 <= hi && lo <= b.x1)
        })
        .collect();
    if !extras.is_empty() {
        bands.extend(column_bands(segs, &extras, s.col_gap));
        bands.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    bands
}

/// 列帯の一致判定
fn bands_match(a: &[(f64, f64)], b: &[(f64, f64)]) -> bool {
    if a.len().min(b.len()) < 2 {
        return false;
    }
    let overlaps = |x: &(f64, f64), y: &(f64, f64)| x.0 <= y.1 && y.0 <= x.1;
    let need = (a.len().max(b.len()) as f64 * 0.8).ceil() as usize;
    let hit_a = a.iter().filter(|x| b.iter().any(|y| overlaps(x, y))).count();
    let hit_b = b.iter().filter(|y| a.iter().any(|x| overlaps(x, y))).count();
    hit_a >= need && hit_b >= need
}

/// 文字整列方式による1ページの表検出
pub fn detect(segs: &[Seg], s: &TextSettings, scale: Option<f64>) -> Vec<Table> {
    // 最小個数ゲートは 1 以上として扱う
    let min_segs = s.min_table_segs.max(1);
    if segs.len() < min_segs {
        return Vec::new();
    }

    let active = connected_set(segs, s.align_tol);
    if active.len() < min_segs {
        return Vec::new();
    }

    let med_h = scale.unwrap_or_else(|| {
        median(active.iter().map(|&i| segs[i].bbox.height()).collect())
    });
    let row_tol = (med_h * 0.5).max(0.5);

    let bottoms: Vec<f64> = active.iter().map(|&i| segs[i].bbox.bottom).collect();
    let row_ids = cluster_ids(&bottoms, row_tol);
    let n_total_rows = row_ids.iter().copied().max().unwrap_or(0) + 1;

    let mut row_top = vec![f64::INFINITY; n_total_rows];
    let mut row_bot = vec![f64::NEG_INFINITY; n_total_rows];
    for (k, &rid) in row_ids.iter().enumerate() {
        let i = active[k];
        if segs[i].bbox.top < row_top[rid] {
            row_top[rid] = segs[i].bbox.top;
        }
        if segs[i].bbox.bottom > row_bot[rid] {
            row_bot[rid] = segs[i].bbox.bottom;
        }
    }

    let mut ordered: Vec<usize> = (0..n_total_rows).collect();
    ordered.sort_by(|&a, &b| row_top[a].total_cmp(&row_top[b]));

    let gaps: Vec<f64> = ordered
        .windows(2)
        .map(|w| row_top[w[1]] - row_bot[w[0]])
        .collect();
    let med_gap = median(gaps);
    let row_gap_threshold = (med_gap * 1.3).max(med_h * 0.5);

    let mut groups: Vec<Vec<usize>> = Vec::new();
    for rid in ordered {
        let split = groups.last().is_none_or(|g| {
            let prev = *g.last().unwrap();
            row_top[rid] - row_bot[prev] > row_gap_threshold
        });
        if split {
            groups.push(vec![rid]);
        } else {
            groups.last_mut().unwrap().push(rid);
        }
    }

    // 見出し行や広い行間で分断された同一表の統合
    let mut is_dropped = vec![true; segs.len()];
    for &i in &active {
        is_dropped[i] = false;
    }
    let members_of = |group: &[usize]| -> Vec<usize> {
        let mut sel = vec![false; n_total_rows];
        for &rid in group {
            sel[rid] = true;
        }
        active
            .iter()
            .zip(row_ids.iter())
            .filter(|&(_, &r)| sel[r])
            .map(|(&i, _)| i)
            .collect()
    };
    let mut merged: Vec<Vec<usize>> = Vec::new();
    for g in groups {
        if let Some(prev) = merged.last_mut() {
            let y0 = row_bot[*prev.last().unwrap()];
            let y1 = row_top[g[0]];
            let pm = members_of(prev);
            let cm = members_of(&g);
            let x0 = pm
                .iter()
                .chain(cm.iter())
                .map(|&i| segs[i].bbox.x0)
                .fold(f64::INFINITY, f64::min);
            let x1 = pm
                .iter()
                .chain(cm.iter())
                .map(|&i| segs[i].bbox.x1)
                .fold(f64::NEG_INFINITY, f64::max);
            let gap_has_dropped = segs.iter().enumerate().any(|(i, sg)| {
                is_dropped[i]
                    && sg.bbox.top > y0
                    && sg.bbox.bottom < y1
                    && sg.bbox.x1 > x0
                    && sg.bbox.x0 < x1
            });
            if (y1 - y0 <= med_gap * 4.0 || gap_has_dropped)
                && bands_match(
                    &column_bands(segs, &pm, s.col_gap),
                    &column_bands(segs, &cm, s.col_gap),
                )
            {
                prev.extend(g);
                continue;
            }
        }
        merged.push(g);
    }

    let mut tables = Vec::new();
    for group in &merged {
        let mut in_group = vec![false; n_total_rows];
        for &rid in group {
            in_group[rid] = true;
        }
        let group_active: Vec<usize> = active
            .iter()
            .zip(row_ids.iter())
            .filter(|&(_, &r)| in_group[r])
            .map(|(&i, _)| i)
            .collect();
        if group_active.len() < min_segs {
            continue;
        }

        // 領域内で落ちた塊の復帰
        let mut bb = segs[group_active[0]].bbox;
        for &i in &group_active[1..] {
            bb = BBox {
                x0: bb.x0.min(segs[i].bbox.x0),
                top: bb.top.min(segs[i].bbox.top),
                x1: bb.x1.max(segs[i].bbox.x1),
                bottom: bb.bottom.max(segs[i].bbox.bottom),
            };
        }
        let mut members = group_active;
        for (i, sg) in segs.iter().enumerate() {
            if is_dropped[i] && bb.contains_center(&sg.bbox) {
                members.push(i);
            }
        }

        let med_h = scale.unwrap_or_else(|| {
            median(members.iter().map(|&i| segs[i].bbox.height()).collect())
        });
        let member_row_tol = (med_h * 0.5).max(0.5);
        let bottoms: Vec<f64> = members.iter().map(|&i| segs[i].bbox.bottom).collect();
        let member_row_ids = cluster_ids(&bottoms, member_row_tol);
        let n_rows = member_row_ids.iter().copied().max().unwrap_or(0) + 1;
        if let Some((t, leader)) = build_table(segs, &members, &member_row_ids, n_rows, s) {
            // 表らしさによる棄却
            if (t.n_rows >= s.untyped_min_rows || has_typed_column(&t))
                && (!prose_heavy(&t, &leader, s) || aligned_full_grid(&t, s))
            {
                tables.push(t);
            }
        }
    }
    tables
}

/// 1つの行グループから1表を組み立てる
///
/// 表とセルごとのリーダー由来フラグを返す
fn build_table(
    segs: &[Seg],
    active: &[usize],
    row_ids: &[usize],
    n_rows: usize,
    s: &TextSettings,
) -> Option<(Table, Vec<Vec<bool>>)> {
    let bands = anchor_column_bands(segs, active, row_ids, n_rows, s);
    let n_cols = bands.len();
    if n_rows < s.min_rows || n_cols < s.min_cols {
        return None;
    }

    // 充填率の上界による早期棄却 非空 text を持つ active セグ数は filled の上界
    let nonempty = active.iter().filter(|&&i| !segs[i].text.is_empty()).count();
    if (nonempty as f64) < s.min_fill * (n_rows as f64) * (n_cols as f64) {
        return None;
    }

    let band_of = |x: f64| -> usize {
        bands
            .iter()
            .position(|b| x >= b.0 && x <= b.1)
            .unwrap_or_else(|| {
                bands
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        let da = (x - (a.0 + a.1) / 2.0).abs();
                        let db = (x - (b.0 + b.1) / 2.0).abs();
                        da.total_cmp(&db)
                    })
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            })
    };

    let mut grid_text: Vec<Vec<String>> = vec![vec![String::new(); n_cols]; n_rows];
    let mut grid_box: Vec<Vec<Option<BBox>>> = vec![vec![None; n_cols]; n_rows];
    let mut grid_leader: Vec<Vec<bool>> = vec![vec![false; n_cols]; n_rows];
    for (k, &i) in active.iter().enumerate() {
        let r = row_ids[k];
        let c = band_of(segs[i].bbox.cx());
        if segs[i].leader_adj {
            grid_leader[r][c] = true;
        }
        if !grid_text[r][c].is_empty() {
            grid_text[r][c].push(' ');
        }
        grid_text[r][c].push_str(&segs[i].text);
        grid_box[r][c] = Some(match grid_box[r][c] {
            Some(b) => BBox {
                x0: b.x0.min(segs[i].bbox.x0),
                top: b.top.min(segs[i].bbox.top),
                x1: b.x1.max(segs[i].bbox.x1),
                bottom: b.bottom.max(segs[i].bbox.bottom),
            },
            None => segs[i].bbox,
        });
    }

    let filled = grid_text.iter().flatten().filter(|t| !t.is_empty()).count();
    if (filled as f64) < s.min_fill * (n_rows as f64) * (n_cols as f64) {
        return None;
    }

    let bbox = BBox {
        x0: active
            .iter()
            .map(|&i| segs[i].bbox.x0)
            .fold(f64::INFINITY, f64::min),
        top: active
            .iter()
            .map(|&i| segs[i].bbox.top)
            .fold(f64::INFINITY, f64::min),
        x1: active
            .iter()
            .map(|&i| segs[i].bbox.x1)
            .fold(f64::NEG_INFINITY, f64::max),
        bottom: active
            .iter()
            .map(|&i| segs[i].bbox.bottom)
            .fold(f64::NEG_INFINITY, f64::max),
    };
    let data: Vec<Vec<Cell>> = (0..n_rows)
        .map(|r| {
            (0..n_cols)
                .map(|c| Cell {
                    text: grid_text[r][c].clone(),
                    bbox: grid_box[r][c].unwrap_or(bbox),
                })
                .collect()
        })
        .collect();

    Some((
        Table {
            extraction_method: "text",
            bbox,
            n_rows,
            n_cols,
            data,
        },
        grid_leader,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 25文字の長文セルで埋めた表を作る x0sは列0の各行の左端
    fn long_text_table(n_rows: usize, x0s: &[f64], fill_all: bool) -> Table {
        let text = "a".repeat(25);
        let mut data = Vec::new();
        for r in 0..n_rows {
            let mut row = Vec::new();
            for c in 0..2 {
                let x0 = if c == 0 { x0s[r % x0s.len()] } else { 200.0 };
                let empty = !fill_all && c == 1 && r % 2 == 0;
                row.push(Cell {
                    text: if empty { String::new() } else { text.clone() },
                    bbox: BBox {
                        x0,
                        top: 10.0 * r as f64,
                        x1: x0 + 100.0,
                        bottom: 10.0 * r as f64 + 8.0,
                    },
                });
            }
            data.push(row);
        }
        Table {
            extraction_method: "text",
            bbox: BBox {
                x0: 0.0,
                top: 0.0,
                x1: 300.0,
                bottom: 10.0 * n_rows as f64,
            },
            n_rows,
            n_cols: 2,
            data,
        }
    }

    #[test]
    fn aligned_full_grid_accepts_aligned_long_text_table() {
        let t = long_text_table(4, &[50.0], true);
        assert!(aligned_full_grid(&t, &TextSettings::default()));
    }

    #[test]
    fn aligned_full_grid_rejects_ragged_columns() {
        let t = long_text_table(4, &[50.0, 58.0, 66.0, 74.0], true);
        assert!(!aligned_full_grid(&t, &TextSettings::default()));
    }

    #[test]
    fn aligned_full_grid_rejects_sparse_fill() {
        let t = long_text_table(4, &[50.0], false);
        assert!(!aligned_full_grid(&t, &TextSettings::default()));
    }

    #[test]
    fn aligned_full_grid_rejects_few_rows() {
        let t = long_text_table(3, &[50.0], true);
        assert!(!aligned_full_grid(&t, &TextSettings::default()));
    }

    /// リーダー由来のセルは prose に数えない
    #[test]
    fn prose_heavy_exempts_leader_cells() {
        let t = long_text_table(4, &[50.0], true);
        let s = TextSettings::default();
        let none = vec![vec![false; 2]; 4];
        assert!(prose_heavy(&t, &none, &s));
        let leader = vec![vec![true; 2]; 4];
        assert!(!prose_heavy(&t, &leader, &s));
    }

    #[test]
    fn cluster_splits_by_tolerance() {
        let ids = cluster_ids(&[0.0, 1.0, 10.0, 10.5], 2.0);
        assert_eq!(ids[0], ids[1]);
        assert_eq!(ids[2], ids[3]);
        assert_ne!(ids[0], ids[2]);
    }

    /// 対角配置で n_rows×n_cols が nonempty を大きく超える入力を早期に棄却する
    #[test]
    fn build_table_rejects_sparse_grid_before_allocation() {
        let n = 20usize;
        let mut segs = Vec::new();
        for k in 0..n {
            segs.push(Seg {
                text: "x".to_string(),
                bbox: BBox {
                    x0: (k as f64) * 20.0,
                    top: (k as f64) * 10.0,
                    x1: (k as f64) * 20.0 + 10.0,
                    bottom: (k as f64) * 10.0 + 8.0,
                },
                leader_adj: false,
            });
        }
        let active: Vec<usize> = (0..n).collect();
        let row_ids: Vec<usize> = (0..n).collect();
        let s = TextSettings::default();
        assert!(build_table(&segs, &active, &row_ids, n, &s).is_none());
    }

    #[test]
    fn group_sizes_counts_members() {
        let sizes = group_sizes(&[0.0, 1.0, 10.0], 2.0);
        assert_eq!(sizes, vec![2, 2, 1]);
    }
}
