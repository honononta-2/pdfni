//! 罫線方式の表検出

use crate::edges_geom::{join_edge_group, snap_edges};
use crate::model::{BBox, Cell, Edge, Orientation, Table};
use std::collections::{HashMap, HashSet};

/// 検出パラメータ
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct LatticeSettings {
    pub snap_tol: f64,
    pub join_tol: f64,
    pub intersection_tol: f64,
    pub min_rows: usize,
    pub min_cols: usize,
    /// 交点計算に入れる線分の最小長
    pub edge_min_length: f64,
    /// 整形前に切る線分の最小長
    pub edge_min_length_prefilter: f64,
}

impl Default for LatticeSettings {
    fn default() -> Self {
        Self {
            snap_tol: 3.0,
            join_tol: 3.0,
            intersection_tol: 3.0,
            min_rows: 2,
            min_cols: 2,
            edge_min_length: 3.0,
            edge_min_length_prefilter: 1.0,
        }
    }
}

/// 点の同一判定に使う量子化キー
fn key(v: f64) -> i64 {
    (v * 100.0).round() as i64
}

/// 交点
#[derive(Default, Clone)]
struct Xsec {
    x: f64,
    y: f64,
    v: HashSet<usize>,
    h: HashSet<usize>,
}

/// 罫線方式による1ページの表検出
/// `fill` はセル矩形内のテキストを返すクロージャ
pub fn detect<F: Fn(&BBox) -> String>(edges: &[Edge], fill: &F, s: &LatticeSettings) -> Vec<Table> {
    if edges.is_empty() {
        return Vec::new();
    }

    let prefiltered: Vec<Edge> = edges
        .iter()
        .copied()
        .filter(|e| e.length() >= s.edge_min_length_prefilter)
        .collect();
    let snapped = snap_edges(prefiltered, s.snap_tol, s.snap_tol);
    let joined = join_edge_group(snapped, s.join_tol, s.join_tol);
    let joined: Vec<Edge> = joined
        .into_iter()
        .filter(|e| e.length() >= s.edge_min_length)
        .collect();

    let completed = complete_outer_borders(&joined, s.intersection_tol);
    let intersections = edges_to_intersections(&completed, s.intersection_tol);
    let cells = intersections_to_cells(&intersections);
    let groups = cells_to_tables(&cells);

    let mut result = Vec::new();
    for group in &groups {
        if let Some(t) = build_table(group, fill, s) {
            result.push(t);
        }
    }
    result
}

/// 縦線と横線の交差判定
fn crosses(v: &Edge, h: &Edge, tol: f64) -> bool {
    v.top <= h.top + tol && v.bottom >= h.top - tol && v.x0 >= h.x0 - tol && v.x0 <= h.x1 + tol
}

/// 罫線網ごとの外接矩形で欠けた外枠を仮想線で補う
/// 外枠のない内側格子だけの表を閉じたセルにするための前処理
fn complete_outer_borders(edges: &[Edge], tol: f64) -> Vec<Edge> {
    let n = edges.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }

    for i in 0..n {
        for j in (i + 1)..n {
            let (a, b) = (&edges[i], &edges[j]);
            let crossed = match (a.orientation, b.orientation) {
                (Orientation::Vertical, Orientation::Horizontal) => crosses(a, b, tol),
                (Orientation::Horizontal, Orientation::Vertical) => crosses(b, a, tol),
                _ => false,
            };
            if crossed {
                let ra = find(&mut parent, i);
                let rb = find(&mut parent, j);
                parent[ra] = rb;
            }
        }
    }

    let mut comps: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        comps.entry(r).or_default().push(i);
    }

    let mut result = edges.to_vec();
    for members in comps.values() {
        let h_cnt = members
            .iter()
            .filter(|&&i| edges[i].orientation == Orientation::Horizontal)
            .count();
        let v_cnt = members.len() - h_cnt;
        if h_cnt == 0 || v_cnt == 0 || members.len() < 3 {
            continue;
        }
        let mut x0 = f64::INFINITY;
        let mut x1 = f64::NEG_INFINITY;
        let mut top = f64::INFINITY;
        let mut bottom = f64::NEG_INFINITY;
        for &i in members {
            x0 = x0.min(edges[i].x0);
            x1 = x1.max(edges[i].x1);
            top = top.min(edges[i].top);
            bottom = bottom.max(edges[i].bottom);
        }

        let has_h_at = |y: f64| {
            members.iter().any(|&i| {
                edges[i].orientation == Orientation::Horizontal && (edges[i].top - y).abs() <= tol
            })
        };
        let has_v_at = |x: f64| {
            members.iter().any(|&i| {
                edges[i].orientation == Orientation::Vertical && (edges[i].x0 - x).abs() <= tol
            })
        };
        if !has_h_at(top) {
            result.push(Edge {
                x0,
                top,
                x1,
                bottom: top,
                orientation: Orientation::Horizontal,
            });
        }
        if !has_h_at(bottom) {
            result.push(Edge {
                x0,
                top: bottom,
                x1,
                bottom,
                orientation: Orientation::Horizontal,
            });
        }
        if !has_v_at(x0) {
            result.push(Edge {
                x0,
                top,
                x1: x0,
                bottom,
                orientation: Orientation::Vertical,
            });
        }
        if !has_v_at(x1) {
            result.push(Edge {
                x0: x1,
                top,
                x1,
                bottom,
                orientation: Orientation::Vertical,
            });
        }
    }
    result
}

/// 交点の抽出
fn edges_to_intersections(edges: &[Edge], tol: f64) -> HashMap<(i64, i64), Xsec> {
    let verticals: Vec<(usize, &Edge)> = edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.orientation == Orientation::Vertical)
        .collect();
    let horizontals: Vec<(usize, &Edge)> = edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.orientation == Orientation::Horizontal)
        .collect();

    let mut map: HashMap<(i64, i64), Xsec> = HashMap::new();
    for (vi, v) in &verticals {
        for (hi, h) in &horizontals {
            if v.top <= h.top + tol
                && v.bottom >= h.top - tol
                && v.x0 >= h.x0 - tol
                && v.x0 <= h.x1 + tol
            {
                let k = (key(v.x0), key(h.top));
                let e = map.entry(k).or_insert_with(|| Xsec {
                    x: v.x0,
                    y: h.top,
                    ..Default::default()
                });
                e.v.insert(*vi);
                e.h.insert(*hi);
            }
        }
    }
    map
}

/// セルの抽出
fn intersections_to_cells(map: &HashMap<(i64, i64), Xsec>) -> Vec<BBox> {
    let mut points: Vec<(i64, i64)> = map.keys().copied().collect();
    points.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let connects = |p1: &(i64, i64), p2: &(i64, i64)| -> bool {
        let a = &map[p1];
        let b = &map[p2];
        if p1.0 == p2.0 && !a.v.is_disjoint(&b.v) {
            return true;
        }
        if p1.1 == p2.1 && !a.h.is_disjoint(&b.h) {
            return true;
        }
        false
    };

    let mut cells = Vec::new();
    for i in 0..points.len() {
        let pt = points[i];
        let rest = &points[i + 1..];
        let below: Vec<(i64, i64)> = rest.iter().copied().filter(|q| q.0 == pt.0).collect();
        let right: Vec<(i64, i64)> = rest.iter().copied().filter(|q| q.1 == pt.1).collect();

        'outer: for bp in &below {
            if !connects(&pt, bp) {
                continue;
            }
            for rp in &right {
                if !connects(&pt, rp) {
                    continue;
                }
                let br = (rp.0, bp.1);
                if map.contains_key(&br) && connects(&br, rp) && connects(&br, bp) {
                    let p = &map[&pt];
                    let b = &map[&br];
                    cells.push(BBox {
                        x0: p.x,
                        top: p.y,
                        x1: b.x,
                        bottom: b.y,
                    });
                    break 'outer;
                }
            }
        }
    }
    cells
}

/// セルを表ごとにまとめる
fn cells_to_tables(cells: &[BBox]) -> Vec<Vec<BBox>> {
    let corners = |c: &BBox| -> [(i64, i64); 4] {
        [
            (key(c.x0), key(c.top)),
            (key(c.x0), key(c.bottom)),
            (key(c.x1), key(c.top)),
            (key(c.x1), key(c.bottom)),
        ]
    };

    let mut remaining: Vec<BBox> = cells.to_vec();
    let mut tables: Vec<Vec<BBox>> = Vec::new();
    let mut cur: Vec<BBox> = Vec::new();
    let mut cur_corners: HashSet<(i64, i64)> = HashSet::new();

    while !remaining.is_empty() {
        let before = cur.len();
        let mut i = 0;
        while i < remaining.len() {
            let cs = corners(&remaining[i]);
            if cur.is_empty() || cs.iter().any(|c| cur_corners.contains(c)) {
                cur_corners.extend(cs);
                cur.push(remaining.remove(i));
            } else {
                i += 1;
            }
        }
        if cur.len() == before {
            if !cur.is_empty() {
                tables.push(std::mem::take(&mut cur));
                cur_corners.clear();
            }
        }
    }
    if !cur.is_empty() {
        tables.push(cur);
    }

    // セル1個だけの塊は表とみなさない
    tables.retain(|t| t.len() > 1);
    tables.sort_by(|a, b| {
        let ka = a.iter().map(|c| (key(c.top), key(c.x0))).min().unwrap();
        let kb = b.iter().map(|c| (key(c.top), key(c.x0))).min().unwrap();
        ka.cmp(&kb)
    });
    tables
}

/// テーブルの整形と出力
fn build_table<F: Fn(&BBox) -> String>(
    cells: &[BBox],
    fill: &F,
    s: &LatticeSettings,
) -> Option<Table> {
    let mut xs: Vec<i64> = cells.iter().map(|c| key(c.x0)).collect();
    xs.sort_unstable();
    xs.dedup();

    let mut by_top: HashMap<i64, Vec<&BBox>> = HashMap::new();
    for c in cells {
        by_top.entry(key(c.top)).or_default().push(c);
    }
    let mut top_keys: Vec<i64> = by_top.keys().copied().collect();
    top_keys.sort_unstable();

    let n_rows = top_keys.len();
    let n_cols = xs.len();
    if n_rows < s.min_rows || n_cols < s.min_cols {
        return None;
    }

    let mut rows: Vec<Vec<Cell>> = Vec::with_capacity(n_rows);
    for tk in &top_keys {
        let row_cells = &by_top[tk];
        let mut row: Vec<Cell> = Vec::with_capacity(n_cols);
        for xk in &xs {
            if let Some(c) = row_cells.iter().find(|c| key(c.x0) == *xk) {
                let text = fill(c);
                row.push(Cell { bbox: **c, text });
            } else {
                // 結合セル等でこの列のセルが無い行は空欄
                row.push(Cell {
                    bbox: BBox {
                        x0: 0.0,
                        top: 0.0,
                        x1: 0.0,
                        bottom: 0.0,
                    },
                    text: String::new(),
                });
            }
        }
        rows.push(row);
    }

    let bbox = BBox {
        x0: cells.iter().map(|c| c.x0).fold(f64::INFINITY, f64::min),
        top: cells.iter().map(|c| c.top).fold(f64::INFINITY, f64::min),
        x1: cells.iter().map(|c| c.x1).fold(f64::NEG_INFINITY, f64::max),
        bottom: cells
            .iter()
            .map(|c| c.bottom)
            .fold(f64::NEG_INFINITY, f64::max),
    };
    Some(Table {
        extraction_method: "lattice",
        bbox,
        n_rows,
        n_cols,
        data: rows,
    })
}
