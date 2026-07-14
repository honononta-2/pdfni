//! 片軸罫線方式の表検出
//! 罫線が縦横どちらか一方しかない表向け

use crate::detect_text::{Seg, cluster_ids, column_bands, group_sizes, is_prose_cell, median};
use crate::edges_geom::{join_edge_group, snap_edges};
use crate::model::{BBox, Cell, Edge, Orientation, Table};
use std::collections::HashSet;

/// 検出パラメータ
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct HybridSettings {
    pub snap_tol: f64,
    pub join_tol: f64,
    /// 区切り線とみなす線分の最小長
    pub edge_min_length: f64,
    /// 領域の外側で区切り線を拾う許容幅
    pub region_pad: f64,
    /// 区切り線が領域の幅・高さと重なるべき最小割合
    pub min_span_ratio: f64,
    /// 罫線軸に必要な区切り線の最小本数
    pub min_lines: usize,
    pub min_rows: usize,
    pub min_cols: usize,
    pub min_fill: f64,
    pub col_gap: f64,
    /// 整列判定の座標許容差
    pub align_tol: f64,
    /// 列帯の根拠とする整列文字塊の最小個数
    pub min_align: usize,
    /// 文章らしいセルの許容割合
    pub max_prose_ratio: f64,
}

impl Default for HybridSettings {
    fn default() -> Self {
        Self {
            snap_tol: 3.0,
            join_tol: 3.0,
            edge_min_length: 3.0,
            region_pad: 8.0,
            min_span_ratio: 0.6,
            min_lines: 2,
            min_rows: 2,
            min_cols: 2,
            min_fill: 0.3,
            col_gap: 1.0,
            align_tol: 1.0,
            min_align: 3,
            max_prose_ratio: 0.4,
        }
    }
}


/// 片軸罫線方式による1ページの表検出
/// `fill` はセル矩形内のテキストを返すクロージャ
/// `nonws_centers` は矩形内の非空白グリフの中心座標列を返すクロージャ
pub fn detect<F, G>(
    segs: &[Seg],
    edges: &[Edge],
    fill: &F,
    nonws_centers: &G,
    s: &HybridSettings,
    scale: Option<f64>,
) -> Vec<Table>
where
    F: Fn(&BBox) -> String,
    G: Fn(&BBox) -> Vec<(f64, f64)>,
{
    if segs.is_empty() || edges.is_empty() {
        return Vec::new();
    }
    let snapped = snap_edges(edges.to_vec(), s.snap_tol, s.snap_tol);
    let joined: Vec<Edge> = join_edge_group(snapped, s.join_tol, s.join_tol)
        .into_iter()
        .filter(|e| e.length() >= s.edge_min_length)
        .collect();
    if joined.is_empty() {
        return Vec::new();
    }

    let mut tables = Vec::new();
    for members in split_regions(segs, scale) {
        if let Some(t) = detect_region(segs, &members, &joined, fill, nonws_centers, s, scale) {
            tables.push(t);
        }
    }
    tables
}

/// 行間ギャップで文字塊を縦の領域に分ける
fn split_regions(segs: &[Seg], scale: Option<f64>) -> Vec<Vec<usize>> {
    let med_h = scale.unwrap_or_else(|| median(segs.iter().map(|g| g.bbox.height()).collect()));
    let row_tol = (med_h * 0.5).max(0.5);
    let bottoms: Vec<f64> = segs.iter().map(|g| g.bbox.bottom).collect();
    let row_ids = cluster_ids(&bottoms, row_tol);
    let n_rows = row_ids.iter().copied().max().unwrap_or(0) + 1;

    let mut row_top = vec![f64::INFINITY; n_rows];
    let mut row_bot = vec![f64::NEG_INFINITY; n_rows];
    for (i, &rid) in row_ids.iter().enumerate() {
        row_top[rid] = row_top[rid].min(segs[i].bbox.top);
        row_bot[rid] = row_bot[rid].max(segs[i].bbox.bottom);
    }
    let mut ordered: Vec<usize> = (0..n_rows).collect();
    ordered.sort_by(|&a, &b| row_top[a].total_cmp(&row_top[b]));

    let gaps: Vec<f64> = ordered
        .windows(2)
        .map(|w| row_top[w[1]] - row_bot[w[0]])
        .collect();
    let med_gap = median(gaps);
    let threshold = (med_gap * 1.3).max(med_h * 0.5);

    let mut group_of = vec![0usize; n_rows];
    let mut n_groups = 0usize;
    for (k, &rid) in ordered.iter().enumerate() {
        if k > 0 && row_top[rid] - row_bot[ordered[k - 1]] > threshold {
            n_groups += 1;
        }
        group_of[rid] = n_groups;
    }

    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); n_groups + 1];
    for (i, &rid) in row_ids.iter().enumerate() {
        groups[group_of[rid]].push(i);
    }
    groups
}

/// 1領域からの表検出
/// 片軸のみに区切り線が揃う場合だけ表を組み立てる
fn detect_region<F, G>(
    segs: &[Seg],
    members: &[usize],
    edges: &[Edge],
    fill: &F,
    nonws_centers: &G,
    s: &HybridSettings,
    scale: Option<f64>,
) -> Option<Table>
where
    F: Fn(&BBox) -> String,
    G: Fn(&BBox) -> Vec<(f64, f64)>,
{
    let mut bbox = segs[*members.first()?].bbox;
    for &i in &members[1..] {
        bbox = BBox {
            x0: bbox.x0.min(segs[i].bbox.x0),
            top: bbox.top.min(segs[i].bbox.top),
            x1: bbox.x1.max(segs[i].bbox.x1),
            bottom: bbox.bottom.max(segs[i].bbox.bottom),
        };
    }

    let overlap = |a0: f64, a1: f64, b0: f64, b1: f64| (a1.min(b1) - a0.max(b0)).max(0.0);
    let hs: Vec<f64> = edges
        .iter()
        .filter(|e| {
            e.orientation == Orientation::Horizontal
                && e.top >= bbox.top - s.region_pad
                && e.top <= bbox.bottom + s.region_pad
                && overlap(e.x0, e.x1, bbox.x0, bbox.x1) >= s.min_span_ratio * bbox.width()
        })
        .map(|e| e.top)
        .collect();
    let vs: Vec<f64> = edges
        .iter()
        .filter(|e| {
            e.orientation == Orientation::Vertical
                && e.x0 >= bbox.x0 - s.region_pad
                && e.x0 <= bbox.x1 + s.region_pad
                && overlap(e.top, e.bottom, bbox.top, bbox.bottom)
                    >= s.min_span_ratio * bbox.height()
        })
        .map(|e| e.x0)
        .collect();

    // 罫線本数の最小ゲートは 1 以上として扱う
    let min_lines = s.min_lines.max(1);
    let (row_bounds, col_bounds) = if hs.len() >= min_lines && vs.len() < min_lines {
        let cols = band_bounds(segs, members, bbox.x0, bbox.x1, s);
        let lines = line_bounds(hs, bbox.top, bbox.bottom);
        let rows = refine_row_bounds(segs, members, lines, &cols, scale);
        (rows, cols)
    } else if vs.len() >= min_lines && hs.len() < min_lines {
        let rows = row_cluster_bounds(segs, members, bbox.top, bbox.bottom, scale);
        let cols = line_bounds(vs, bbox.x0, bbox.x1);
        (rows, cols)
    } else {
        return None;
    };

    let n_rows = row_bounds.len().saturating_sub(1);
    let n_cols = col_bounds.len().saturating_sub(1);
    if n_rows < s.min_rows || n_cols < s.min_cols {
        return None;
    }

    // 充填率の上界による早期棄却 非空白グリフ中心の占有セル数は filled の上界
    let outer = BBox {
        x0: col_bounds[0],
        top: row_bounds[0],
        x1: *col_bounds.last().unwrap(),
        bottom: *row_bounds.last().unwrap(),
    };
    let mut occupied: HashSet<(usize, usize)> = HashSet::new();
    for (cx, cy) in nonws_centers(&outer) {
        let ri = row_bounds.partition_point(|&b| b <= cy);
        if ri == 0 || ri > n_rows {
            continue;
        }
        let ci = col_bounds.partition_point(|&b| b <= cx);
        if ci == 0 || ci > n_cols {
            continue;
        }
        occupied.insert((ri - 1, ci - 1));
    }
    if (occupied.len() as f64) < s.min_fill * (n_rows as f64) * (n_cols as f64) {
        return None;
    }

    let mut data = Vec::with_capacity(n_rows);
    let mut filled = 0usize;
    for r in 0..n_rows {
        let mut row = Vec::with_capacity(n_cols);
        for c in 0..n_cols {
            let cb = BBox {
                x0: col_bounds[c],
                top: row_bounds[r],
                x1: col_bounds[c + 1],
                bottom: row_bounds[r + 1],
            };
            let text = fill(&cb);
            if !text.trim().is_empty() {
                filled += 1;
            }
            row.push(Cell { bbox: cb, text });
        }
        data.push(row);
    }
    if (filled as f64) < s.min_fill * (n_rows as f64) * (n_cols as f64) {
        return None;
    }
    // 文章セル比率による棄却
    let prose = data
        .iter()
        .flatten()
        .filter(|c| is_prose_cell(&c.text))
        .count();
    if (prose as f64) > s.max_prose_ratio * filled as f64 {
        return None;
    }

    Some(Table {
        extraction_method: "hybrid",
        bbox: BBox {
            x0: col_bounds[0],
            top: row_bounds[0],
            x1: *col_bounds.last().unwrap(),
            bottom: *row_bounds.last().unwrap(),
        },
        n_rows,
        n_cols,
        data,
    })
}

/// 区切り座標列を非減少に補正する
fn monotonize(bounds: &mut [f64]) {
    for i in 1..bounds.len() {
        if bounds[i] < bounds[i - 1] {
            bounds[i] = bounds[i - 1];
        }
    }
}

/// 罫線位置から区切り座標列を作る
/// 文字が最外の線からはみ出す場合は端に仮想の区切りを足す
fn line_bounds(mut pos: Vec<f64>, lo: f64, hi: f64) -> Vec<f64> {
    pos.sort_by(|a, b| a.total_cmp(b));
    pos.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    let mut bounds = Vec::with_capacity(pos.len() + 2);
    if lo < pos[0] - 1.0 {
        bounds.push(lo - 0.5);
    }
    bounds.extend(pos);
    if hi > *bounds.last().unwrap() + 1.0 {
        bounds.push(hi + 0.5);
    }
    bounds
}

/// 文字塊の列帯から区切り座標列を作る
/// 列帯は他の文字塊と整列している塊だけから作り、揃わない塊は境界の根拠にしない
fn band_bounds(segs: &[Seg], members: &[usize], lo: f64, hi: f64, s: &HybridSettings) -> Vec<f64> {
    let lefts: Vec<f64> = members.iter().map(|&i| segs[i].bbox.x0).collect();
    let rights: Vec<f64> = members.iter().map(|&i| segs[i].bbox.x1).collect();
    let mids: Vec<f64> = members.iter().map(|&i| segs[i].bbox.cx()).collect();
    let gl = group_sizes(&lefts, s.align_tol);
    let gr = group_sizes(&rights, s.align_tol);
    let gm = group_sizes(&mids, s.align_tol);
    let aligned: Vec<usize> = (0..members.len())
        .filter(|&k| gl[k].max(gr[k]).max(gm[k]) >= s.min_align)
        .map(|k| members[k])
        .collect();
    let use_members = if aligned.is_empty() { members } else { &aligned[..] };
    let bands = column_bands(segs, use_members, s.col_gap);
    let mut bounds = Vec::with_capacity(bands.len() + 1);
    bounds.push(lo - 0.5);
    for w in bands.windows(2) {
        bounds.push((w[0].1 + w[1].0) / 2.0);
    }
    bounds.push(hi + 0.5);
    bounds
}

/// 罫線間の行帯を文字行で細分した区切り座標列
///
/// 複列クラスタを論理行の核とし 単列クラスタは最近傍の核へ帰属させる
/// ラベル折返し断片が直前行へ誤結合するのを避ける
fn refine_row_bounds(
    segs: &[Seg],
    members: &[usize],
    bounds: Vec<f64>,
    col_bounds: &[f64],
    scale: Option<f64>,
) -> Vec<f64> {
    let med_h = scale
        .unwrap_or_else(|| median(members.iter().map(|&i| segs[i].bbox.height()).collect()));
    let row_tol = (med_h * 0.5).max(0.5);
    let col_of = |x: f64| col_bounds.iter().take_while(|&&b| b < x).count();

    let mut out = Vec::with_capacity(bounds.len());
    for w in bounds.windows(2) {
        out.push(w[0]);
        let inside: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&i| {
                let cy = segs[i].bbox.cy();
                cy >= w[0] && cy < w[1]
            })
            .collect();
        if inside.len() < 2 {
            continue;
        }
        let bottoms: Vec<f64> = inside.iter().map(|&i| segs[i].bbox.bottom).collect();
        let ids = cluster_ids(&bottoms, row_tol);
        let n = ids.iter().copied().max().unwrap_or(0) + 1;
        if n < 2 {
            continue;
        }
        let mut cols: Vec<HashSet<usize>> = vec![HashSet::new(); n];
        let mut top = vec![f64::INFINITY; n];
        let mut bot = vec![f64::NEG_INFINITY; n];
        for (k, &rid) in ids.iter().enumerate() {
            let b = &segs[inside[k]].bbox;
            cols[rid].insert(col_of(b.cx()));
            top[rid] = top[rid].min(b.top);
            bot[rid] = bot[rid].max(b.bottom);
        }
        let anchors: Vec<usize> = (0..n).filter(|&i| cols[i].len() >= 2).collect();
        if anchors.len() < 2 {
            continue;
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| top[a].total_cmp(&top[b]));
        let anchors: Vec<usize> = order
            .iter()
            .copied()
            .filter(|&i| cols[i].len() >= 2)
            .collect();

        let mut owner = vec![0usize; n];
        for &a in &anchors {
            owner[a] = a;
        }
        for &cid in &order {
            if cols[cid].len() >= 2 {
                continue;
            }
            let cy = (top[cid] + bot[cid]) * 0.5;
            let mut best = anchors[0];
            let mut best_d = f64::INFINITY;
            for &a in &anchors {
                let ay = (top[a] + bot[a]) * 0.5;
                let d = (cy - ay).abs();
                if d < best_d {
                    best_d = d;
                    best = a;
                }
            }
            owner[cid] = best;
        }

        for aw in anchors.windows(2) {
            let g0_bot = (0..n)
                .filter(|&i| owner[i] == aw[0])
                .map(|i| bot[i])
                .fold(f64::NEG_INFINITY, f64::max);
            let g1_top = (0..n)
                .filter(|&i| owner[i] == aw[1])
                .map(|i| top[i])
                .fold(f64::INFINITY, f64::min);
            out.push((g0_bot + g1_top) * 0.5);
        }
    }
    out.push(*bounds.last().unwrap());
    monotonize(&mut out);
    out
}

/// 文字塊の行クラスタから区切り座標列を作る
fn row_cluster_bounds(
    segs: &[Seg],
    members: &[usize],
    lo: f64,
    hi: f64,
    scale: Option<f64>,
) -> Vec<f64> {
    let med_h = scale
        .unwrap_or_else(|| median(members.iter().map(|&i| segs[i].bbox.height()).collect()));
    let row_tol = (med_h * 0.5).max(0.5);
    let bottoms: Vec<f64> = members.iter().map(|&i| segs[i].bbox.bottom).collect();
    let ids = cluster_ids(&bottoms, row_tol);
    let n = ids.iter().copied().max().unwrap_or(0) + 1;

    let mut top = vec![f64::INFINITY; n];
    let mut bot = vec![f64::NEG_INFINITY; n];
    for (k, &rid) in ids.iter().enumerate() {
        let b = &segs[members[k]].bbox;
        top[rid] = top[rid].min(b.top);
        bot[rid] = bot[rid].max(b.bottom);
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| top[a].total_cmp(&top[b]));

    let mut bounds = Vec::with_capacity(n + 1);
    bounds.push(lo - 0.5);
    for w in order.windows(2) {
        bounds.push((bot[w[0]] + top[w[1]]) / 2.0);
    }
    bounds.push(hi + 0.5);
    monotonize(&mut bounds);
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str, x0: f64, x1: f64, top: f64, bottom: f64) -> Seg {
        Seg {
            text: text.to_string(),
            bbox: BBox { x0, top, x1, bottom },
        }
    }

    /// 背の高いグリフを含む入力で行クラスタ境界が非減少に保たれる
    #[test]
    fn row_cluster_bounds_is_monotonic_with_drop_cap() {
        let segs = vec![
            seg("a", 20.0, 30.0, 0.0, 10.0),
            seg("b", 20.0, 30.0, 20.0, 30.0),
            seg("c", 20.0, 30.0, 40.0, 50.0),
            seg("d", 20.0, 30.0, 60.0, 70.0),
            seg("D", 0.0, 15.0, 0.0, 100.0),
        ];
        let members: Vec<usize> = (0..segs.len()).collect();
        let bounds = row_cluster_bounds(&segs, &members, 0.0, 100.0, Some(10.0));
        for w in bounds.windows(2) {
            assert!(w[0] <= w[1], "bounds not monotonic: {:?}", bounds);
        }
    }

    #[test]
    fn monotonize_flattens_inversions() {
        let mut v = vec![-0.5, 5.0, 60.0, 35.0, 55.0, 100.5];
        monotonize(&mut v);
        assert_eq!(v, vec![-0.5, 5.0, 60.0, 60.0, 60.0, 100.5]);
    }

    /// min_lines = 0 でも罫線本数ゲートを 1 として扱い hybrid が検出される
    #[test]
    fn detect_region_zero_min_lines_enters_branch() {
        let segs = vec![
            seg("a", 0.0, 10.0, 0.0, 10.0),
            seg("b", 15.0, 25.0, 0.0, 10.0),
            seg("c", 0.0, 10.0, 20.0, 30.0),
            seg("d", 15.0, 25.0, 20.0, 30.0),
        ];
        let members: Vec<usize> = (0..segs.len()).collect();
        let edges = vec![
            Edge {
                x0: 12.0,
                x1: 12.0,
                top: 0.0,
                bottom: 30.0,
                orientation: Orientation::Vertical,
            },
            Edge {
                x0: 22.0,
                x1: 22.0,
                top: 0.0,
                bottom: 30.0,
                orientation: Orientation::Vertical,
            },
        ];
        let fill = |_: &BBox| "x".to_string();
        let nonws_centers =
            |_: &BBox| vec![(5.0, 5.0), (18.0, 5.0), (5.0, 25.0), (18.0, 25.0)];
        let mut s = HybridSettings::default();
        s.min_lines = 0;
        let result =
            detect_region(&segs, &members, &edges, &fill, &nonws_centers, &s, Some(10.0));
        assert!(
            result.is_some(),
            "min_lines=0 must be normalized to 1 and enter the vertical branch"
        );
    }

    /// nonws グリフ中心が皆無なら全セル fill を通さずに棄却する
    #[test]
    fn detect_region_rejects_when_no_nonws_centers() {
        let segs = vec![
            seg("a", 0.0, 10.0, 0.0, 10.0),
            seg("b", 0.0, 10.0, 20.0, 30.0),
            seg("c", 0.0, 10.0, 40.0, 50.0),
            seg("d", 0.0, 10.0, 60.0, 70.0),
        ];
        let members: Vec<usize> = (0..segs.len()).collect();
        let edges = vec![
            Edge {
                x0: 5.0,
                x1: 5.0,
                top: 0.0,
                bottom: 70.0,
                orientation: Orientation::Vertical,
            },
            Edge {
                x0: 12.0,
                x1: 12.0,
                top: 0.0,
                bottom: 70.0,
                orientation: Orientation::Vertical,
            },
        ];
        let fill = |_: &BBox| "x".to_string();
        let nonws_centers = |_: &BBox| Vec::<(f64, f64)>::new();
        let s = HybridSettings::default();
        let result = detect_region(&segs, &members, &edges, &fill, &nonws_centers, &s, Some(10.0));
        assert!(result.is_none());
    }
}
