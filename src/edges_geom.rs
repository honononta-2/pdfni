//! 罫線エッジのジオメトリ整形

use crate::model::{Edge, Orientation};

/// 近接する平行エッジを同じ位置に揃える
pub fn snap_edges(edges: Vec<Edge>, snap_x_tolerance: f64, snap_y_tolerance: f64) -> Vec<Edge> {
    let mut result = Vec::with_capacity(edges.len());
    let mut horizontals: Vec<Edge> = Vec::new();
    let mut verticals: Vec<Edge> = Vec::new();

    for edge in edges {
        match edge.orientation {
            Orientation::Horizontal => horizontals.push(edge),
            Orientation::Vertical => verticals.push(edge),
        }
    }

    snap_group(
        &mut horizontals,
        snap_y_tolerance,
        |e| e.top,
        |e, v| {
            e.top = v;
            e.bottom = v;
        },
    );
    result.extend(horizontals);

    snap_group(
        &mut verticals,
        snap_x_tolerance,
        |e| e.x0,
        |e, v| {
            e.x0 = v;
            e.x1 = v;
        },
    );
    result.extend(verticals);

    result
}

/// 1軸の近接クラスタを平均位置へ揃える
fn snap_group<F, G>(edges: &mut [Edge], tolerance: f64, key: F, mut set: G)
where
    F: Fn(&Edge) -> f64,
    G: FnMut(&mut Edge, f64),
{
    if edges.is_empty() {
        return;
    }

    edges.sort_by(|a, b| key(a).total_cmp(&key(b)));

    let mut cluster_start = 0;
    let mut prev = key(&edges[0]);
    for i in 1..=edges.len() {
        let end_of_cluster = i == edges.len() || (key(&edges[i]) - prev).abs() > tolerance;
        if end_of_cluster {
            let sum: f64 = (cluster_start..i).map(|j| key(&edges[j])).sum();
            let mean = sum / (i - cluster_start) as f64;
            for edge in &mut edges[cluster_start..i] {
                set(edge, mean);
            }
            cluster_start = i;
        }
        if i < edges.len() {
            prev = key(&edges[i]);
        }
    }
}

/// 同一直線上で重なる・隣接する線分を1本に連結する
pub fn join_edge_group(
    edges: Vec<Edge>,
    join_x_tolerance: f64,
    join_y_tolerance: f64,
) -> Vec<Edge> {
    let mut result: Vec<Edge> = Vec::new();
    let mut horizontals: Vec<Edge> = Vec::new();
    let mut verticals: Vec<Edge> = Vec::new();

    for edge in edges {
        match edge.orientation {
            Orientation::Horizontal => horizontals.push(edge),
            Orientation::Vertical => verticals.push(edge),
        }
    }

    result.extend(join_collinear(
        horizontals,
        |e| e.top,
        |e| (e.x0, e.x1),
        |proto, start, end| Edge {
            x0: start,
            top: proto.top,
            x1: end,
            bottom: proto.bottom,
            orientation: proto.orientation,
        },
        join_x_tolerance,
    ));

    result.extend(join_collinear(
        verticals,
        |e| e.x0,
        |e| (e.top, e.bottom),
        |proto, start, end| Edge {
            x0: proto.x0,
            top: start,
            x1: proto.x1,
            bottom: end,
            orientation: proto.orientation,
        },
        join_y_tolerance,
    ));

    result
}

/// 同一直線キーごとの線分連結
fn join_collinear<K, S, B>(
    mut edges: Vec<Edge>,
    key: K,
    span: S,
    build: B,
    tolerance: f64,
) -> Vec<Edge>
where
    K: Fn(&Edge) -> f64,
    S: Fn(&Edge) -> (f64, f64),
    B: Fn(&Edge, f64, f64) -> Edge,
{
    if edges.is_empty() {
        return Vec::new();
    }

    edges.sort_by(|a, b| {
        key(a)
            .total_cmp(&key(b))
            .then_with(|| span(a).0.total_cmp(&span(b).0))
    });

    let mut result = Vec::new();
    let mut i = 0;

    while i < edges.len() {
        let group_key = key(&edges[i]);
        let mut j = i + 1;
        while j < edges.len() && (key(&edges[j]) - group_key).abs() < 1e-9 {
            j += 1;
        }

        let (mut cur_start, mut cur_end) = span(&edges[i]);
        let mut proto_idx = i;

        for k in (i + 1)..j {
            let (s, e) = span(&edges[k]);
            if s <= cur_end + tolerance {
                if e > cur_end {
                    cur_end = e;
                }
            } else {
                result.push(build(&edges[proto_idx], cur_start, cur_end));
                cur_start = s;
                cur_end = e;
                proto_idx = k;
            }
        }
        result.push(build(&edges[proto_idx], cur_start, cur_end));

        i = j;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hedge(top: f64, x0: f64, x1: f64) -> Edge {
        Edge {
            x0,
            top,
            x1,
            bottom: top,
            orientation: Orientation::Horizontal,
        }
    }

    #[test]
    fn snap_aligns_near_parallel() {
        let out = snap_edges(
            vec![hedge(100.0, 0.0, 50.0), hedge(101.5, 0.0, 50.0)],
            3.0,
            3.0,
        );
        assert!((out[0].top - out[1].top).abs() < 1e-9);
    }

    #[test]
    fn snap_keeps_distant_apart() {
        let out = snap_edges(
            vec![hedge(100.0, 0.0, 50.0), hedge(110.0, 0.0, 50.0)],
            3.0,
            3.0,
        );
        assert!((out[0].top - out[1].top).abs() > 1.0);
    }

    #[test]
    fn join_connects_collinear() {
        let out = join_edge_group(
            vec![hedge(100.0, 0.0, 30.0), hedge(100.0, 31.0, 60.0)],
            3.0,
            3.0,
        );
        assert_eq!(out.len(), 1);
        assert!((out[0].x1 - 60.0).abs() < 1e-9);
    }
}
