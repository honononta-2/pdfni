//! 段組検出の領域分割

use std::collections::HashSet;

/// X分割の valley 最小幅（pt）
const X_VALLEY_MIN_WIDTH: f64 = 10.0;
/// X分割の側の最小行数
const X_SIDE_MIN_LINES: usize = 5;
/// X分割の側の最小密度
const X_SIDE_MIN_DENSITY: f64 = 0.3;
/// X分割の左右内容幅比の下限（狭い方 / 広い方）
const X_WIDTH_RATIO_MIN: f64 = 0.2;
/// 仮 gutter の最小幅（pt）
const Y_GUTTER_MIN_WIDTH: f64 = 10.0;
/// 原子帯との交差とみなす重なり幅の下限（pt）
const FW_CROSS_TOLERANCE: f64 = 0.5;
/// 全幅要素の対象本文行数比の上限（これを超えると Y分割不成立）
const FULLWIDTH_LINE_RATIO_MAX: f64 = 0.3;
/// 再帰分割の深さ上限（この深さの領域は葉）
const MAX_DEPTH: usize = 8;

/// 参加要素の種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParticipantKind {
    /// 対象本文行の単語。line は行の識別番号（行数の数え上げと行への一意化に使う）
    Word { line: usize },
    Table,
    OtherLine,
}

/// 領域分割の参加要素
#[derive(Debug, Clone, Copy)]
pub(crate) struct Participant {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub kind: ParticipantKind,
}

/// ページを領域分割し、各参加要素の葉領域番号（読み順）を返す
/// X分割が1つも発生しない場合は None（呼び出し側は従来経路をそのまま使う）
pub(crate) fn split_regions(
    page_width: f64,
    page_height: f64,
    parts: &[Participant],
) -> Option<Vec<usize>> {
    if parts.is_empty() {
        return None;
    }
    let indices: Vec<usize> = (0..parts.len()).collect();
    let tree = build_tree(
        parts,
        &indices,
        Rect {
            left: 0.0,
            right: page_width,
            top: 0.0,
            bottom: page_height,
        },
        0,
    );
    let tree = fold_tree(tree);
    if !tree_has_x(&tree) {
        return None;
    }
    let mut leaf_of = vec![0usize; parts.len()];
    let mut next_leaf = 0usize;
    assign_leaves(&tree, &mut leaf_of, &mut next_leaf);
    Some(leaf_of)
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

/// 開区間の valley
#[derive(Debug, Clone, Copy)]
struct Valley {
    start: f64,
    end: f64,
}

impl Valley {
    fn width(self) -> f64 {
        self.end - self.start
    }

    fn mid(self) -> f64 {
        self.start + (self.end - self.start) / 2.0
    }
}

/// 閉区間（投影）
#[derive(Debug, Clone, Copy)]
struct Interval {
    start: f64,
    end: f64,
}

/// 領域木
enum Tree {
    Leaf {
        parts: Vec<usize>,
    },
    Split {
        is_x: bool,
        left: Box<Tree>,
        right: Box<Tree>,
    },
}

/// 全幅要素の y 区間
#[derive(Debug, Clone, Copy)]
struct FwElem {
    id: usize,
    top: f64,
    bottom: f64,
    /// 対象本文行なら Some
    body_line: Option<usize>,
}

/// 全幅ブロックの y 区間
#[derive(Debug, Clone, Copy)]
struct FwBlock {
    top: f64,
    bottom: f64,
}

fn is_valid_bbox(p: &Participant) -> bool {
    let w = p.right - p.left;
    let h = p.bottom - p.top;
    w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0
}

fn kind_ord(k: ParticipantKind) -> u8 {
    match k {
        ParticipantKind::Word { .. } => 0,
        ParticipantKind::Table => 1,
        ParticipantKind::OtherLine => 2,
    }
}

/// 軸へ射影した閉区間を整列・併合する
fn project_intervals(parts: &[Participant], ids: &[usize], axis_x: bool) -> Vec<Interval> {
    let mut segs: Vec<(f64, f64, u8, usize)> = Vec::new();
    for &id in ids {
        let p = &parts[id];
        if !is_valid_bbox(p) {
            continue;
        }
        let (s, e) = if axis_x {
            (p.left, p.right)
        } else {
            (p.top, p.bottom)
        };
        segs.push((s, e, kind_ord(p.kind), id));
    }
    segs.sort_by(|a, b| {
        a.0.total_cmp(&b.0)
            .then_with(|| a.1.total_cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| a.3.cmp(&b.3))
    });
    let mut merged: Vec<Interval> = Vec::new();
    for (s, e, _, _) in segs {
        if let Some(last) = merged.last_mut() {
            if s <= last.end {
                if e > last.end {
                    last.end = e;
                }
                continue;
            }
        }
        merged.push(Interval { start: s, end: e });
    }
    merged
}

/// 内部空帯のみの valley 列
fn valleys_from_intervals(intervals: &[Interval]) -> Vec<Valley> {
    let mut out = Vec::new();
    for w in intervals.windows(2) {
        let start = w[0].end;
        let end = w[1].start;
        if end > start {
            out.push(Valley { start, end });
        }
    }
    out
}

/// 所属判定: true なら左（上）、false なら右（下）
fn on_low_side(p: &Participant, v: Valley, axis_x: bool) -> bool {
    let (start, end) = if axis_x {
        (p.left, p.right)
    } else {
        (p.top, p.bottom)
    };
    if end <= v.start {
        return true;
    }
    if start >= v.end {
        return false;
    }
    let center = (start + end) / 2.0;
    let vmid = v.mid();
    if !center.is_finite() || !vmid.is_finite() {
        return true;
    }
    center.total_cmp(&vmid) != std::cmp::Ordering::Greater
}

fn split_ids(
    parts: &[Participant],
    ids: &[usize],
    v: Valley,
    axis_x: bool,
) -> (Vec<usize>, Vec<usize>) {
    let mut lo = Vec::new();
    let mut hi = Vec::new();
    for &id in ids {
        if on_low_side(&parts[id], v, axis_x) {
            lo.push(id);
        } else {
            hi.push(id);
        }
    }
    (lo, hi)
}

/// 側の対象本文行数・内容幅・密度
fn side_metrics(parts: &[Participant], ids: &[usize]) -> (usize, f64, f64) {
    let mut lines = Vec::new();
    let mut area_sum = 0.0;
    let mut union_l = f64::INFINITY;
    let mut union_r = f64::NEG_INFINITY;
    let mut union_t = f64::INFINITY;
    let mut union_b = f64::NEG_INFINITY;
    let mut any = false;
    for &id in ids {
        let p = &parts[id];
        let ParticipantKind::Word { line } = p.kind else {
            continue;
        };
        if !is_valid_bbox(p) {
            continue;
        }
        lines.push(line);
        area_sum += (p.right - p.left) * (p.bottom - p.top);
        union_l = union_l.min(p.left);
        union_r = union_r.max(p.right);
        union_t = union_t.min(p.top);
        union_b = union_b.max(p.bottom);
        any = true;
    }
    lines.sort_unstable();
    lines.dedup();
    let line_count = lines.len();
    if !any {
        return (line_count, 0.0, 0.0);
    }
    let content_w = union_r - union_l;
    let content_h = union_b - union_t;
    let content_area = content_w * content_h;
    let density = if content_area > 0.0 && content_area.is_finite() {
        (area_sum / content_area).min(1.0)
    } else {
        0.0
    };
    (line_count, content_w, density)
}

fn try_x_split(parts: &[Participant], ids: &[usize]) -> Option<Valley> {
    let intervals = project_intervals(parts, ids, true);
    let valleys = valleys_from_intervals(&intervals);
    let mut best: Option<Valley> = None;
    for v in valleys {
        if v.width() < X_VALLEY_MIN_WIDTH {
            continue;
        }
        let (left_ids, right_ids) = split_ids(parts, ids, v, true);
        let (llines, lwidth, ldens) = side_metrics(parts, &left_ids);
        let (rlines, rwidth, rdens) = side_metrics(parts, &right_ids);
        if llines < X_SIDE_MIN_LINES || rlines < X_SIDE_MIN_LINES {
            continue;
        }
        if ldens < X_SIDE_MIN_DENSITY || rdens < X_SIDE_MIN_DENSITY {
            continue;
        }
        let (narrow, wide) = if lwidth <= rwidth {
            (lwidth, rwidth)
        } else {
            (rwidth, lwidth)
        };
        if wide <= 0.0 || !wide.is_finite() {
            continue;
        }
        if narrow / wide < X_WIDTH_RATIO_MIN {
            continue;
        }
        match best {
            None => best = Some(v),
            Some(b) => {
                let cmp = v.width().total_cmp(&b.width());
                if cmp == std::cmp::Ordering::Greater
                    || (cmp == std::cmp::Ordering::Equal
                        && v.start.total_cmp(&b.start) == std::cmp::Ordering::Less)
                {
                    best = Some(v);
                }
            }
        }
    }
    best
}

/// 行 bbox（行→単語索引から和集合）
fn line_bbox_indexed(
    parts: &[Participant],
    line_words: &[Vec<usize>],
    line: usize,
) -> Option<(f64, f64, f64, f64)> {
    if line >= line_words.len() {
        return None;
    }
    let mut l = f64::INFINITY;
    let mut r = f64::NEG_INFINITY;
    let mut t = f64::INFINITY;
    let mut b = f64::NEG_INFINITY;
    let mut any = false;
    for &id in &line_words[line] {
        let p = &parts[id];
        if !is_valid_bbox(p) {
            continue;
        }
        l = l.min(p.left);
        r = r.max(p.right);
        t = t.min(p.top);
        b = b.max(p.bottom);
        any = true;
    }
    if any {
        Some((l, r, t, b))
    } else {
        None
    }
}

fn body_line_count_indexed(line_words: &[Vec<usize>]) -> usize {
    line_words.iter().filter(|w| !w.is_empty()).count()
}

/// 領域内の行→単語ID索引
fn build_line_words(parts: &[Participant], ids: &[usize]) -> Vec<Vec<usize>> {
    let mut max_line = 0usize;
    let mut any = false;
    for &id in ids {
        if let ParticipantKind::Word { line } = parts[id].kind {
            max_line = max_line.max(line);
            any = true;
        }
    }
    if !any {
        return Vec::new();
    }
    let mut line_words = vec![Vec::new(); max_line + 1];
    for &id in ids {
        if let ParticipantKind::Word { line } = parts[id].kind {
            line_words[line].push(id);
        }
    }
    line_words
}

/// F(B) を行昇格した F'(B) と対象本文行数
fn promote_f_prime(
    parts: &[Participant],
    f: &[usize],
    line_words: &[Vec<usize>],
    remove: &mut [bool],
) -> usize {
    remove.fill(false);
    let mut body_lines = 0usize;
    let mut seen_line = vec![false; line_words.len()];
    for &id in f {
        match parts[id].kind {
            ParticipantKind::Word { line } => {
                if line < seen_line.len() && !seen_line[line] {
                    seen_line[line] = true;
                    body_lines += 1;
                    for &wid in &line_words[line] {
                        remove[wid] = true;
                    }
                }
            }
            ParticipantKind::Table | ParticipantKind::OtherLine => {
                remove[id] = true;
            }
        }
    }
    body_lines
}

/// 合流後の1候補 (a, b, F) の受理判定と全幅マークの更新
fn evaluate_fw_candidate(
    parts: &[Participant],
    line_words: &[Vec<usize>],
    region_lines: usize,
    ids: &[usize],
    a: f64,
    b: f64,
    f: &[usize],
    remove: &mut [bool],
    fw_mark: &mut [bool],
) -> bool {
    if f.is_empty() {
        return false;
    }
    let fw_body = promote_f_prime(parts, f, line_words, remove);
    if region_lines > 0 {
        let ratio = fw_body as f64 / region_lines as f64;
        if ratio > FULLWIDTH_LINE_RATIO_MAX {
            return false;
        }
    }
    let remain: Vec<usize> = ids.iter().copied().filter(|&id| !remove[id]).collect();
    let intervals = project_intervals(parts, &remain, true);
    if intervals.len() < 2 {
        return false;
    }
    let band_in_valley = valleys_from_intervals(&intervals)
        .into_iter()
        .any(|v| v.width() >= Y_GUTTER_MIN_WIDTH && v.start <= a && v.end >= b);
    if !band_in_valley {
        return false;
    }
    for (id, &rm) in remove.iter().enumerate() {
        if rm {
            fw_mark[id] = true;
        }
    }
    true
}

fn try_y_split(parts: &[Participant], ids: &[usize]) -> Option<Valley> {
    let line_words = build_line_words(parts, ids);
    let region_lines = body_line_count_indexed(&line_words);

    let mut ends = Vec::new();
    let mut events: Vec<(f64, i32, usize)> = Vec::new();
    for &id in ids {
        let p = &parts[id];
        if !is_valid_bbox(p) {
            continue;
        }
        ends.push(p.left);
        ends.push(p.right);
        events.push((p.left, 1, id));
        events.push((p.right, -1, id));
    }
    ends.sort_by(|a, b| a.total_cmp(b));
    ends.dedup_by(|a, b| a.total_cmp(b) == std::cmp::Ordering::Equal);
    if ends.len() < 2 {
        return None;
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));

    let n_parts = parts.len();
    let mut remove = vec![false; n_parts];
    let mut fw_mark = vec![false; n_parts];
    let mut accepted_any = false;

    // 原子帯の F(B) をイベントスイープで差分更新し 合流した候補をその場で評価
    let mut active: HashSet<usize> = HashSet::new();
    let mut event_idx = 0usize;
    let mut cur_a: f64 = 0.0;
    let mut cur_b: f64 = 0.0;
    let mut cur_f: Vec<usize> = Vec::new();
    let mut has_cur = false;

    for w in ends.windows(2) {
        let a = w[0];
        let b = w[1];
        if b.total_cmp(&a) != std::cmp::Ordering::Greater {
            continue;
        }
        while event_idx < events.len()
            && events[event_idx].0.total_cmp(&a) != std::cmp::Ordering::Greater
        {
            let (_, delta, id) = events[event_idx];
            if delta > 0 {
                active.insert(id);
            } else {
                active.remove(&id);
            }
            event_idx += 1;
        }
        // 帯幅が許容以下なら現行の overlap 判定と等価に F を空扱い
        let f: Vec<usize> = if b - a > FW_CROSS_TOLERANCE {
            ids.iter().copied().filter(|id| active.contains(id)).collect()
        } else {
            Vec::new()
        };
        if has_cur && f == cur_f {
            cur_b = b;
            continue;
        }
        if has_cur
            && evaluate_fw_candidate(
                parts,
                &line_words,
                region_lines,
                ids,
                cur_a,
                cur_b,
                &cur_f,
                &mut remove,
                &mut fw_mark,
            )
        {
            accepted_any = true;
        }
        cur_a = a;
        cur_b = b;
        cur_f = f;
        has_cur = true;
    }
    if has_cur
        && evaluate_fw_candidate(
            parts,
            &line_words,
            region_lines,
            ids,
            cur_a,
            cur_b,
            &cur_f,
            &mut remove,
            &mut fw_mark,
        )
    {
        accepted_any = true;
    }
    if !accepted_any {
        return None;
    }

    // 全幅要素へ一意化（単語 → 行 bbox）
    let mut fw_elems: Vec<FwElem> = Vec::new();
    let mut seen_lines = vec![false; line_words.len()];
    for &id in ids {
        if !fw_mark[id] {
            continue;
        }
        let p = &parts[id];
        match p.kind {
            ParticipantKind::Word { line } => {
                if line >= seen_lines.len() || seen_lines[line] {
                    continue;
                }
                seen_lines[line] = true;
                let Some((_l, _r, t, b)) = line_bbox_indexed(parts, &line_words, line) else {
                    continue;
                };
                let rep = line_words[line].iter().copied().min().unwrap_or(id);
                fw_elems.push(FwElem {
                    id: rep,
                    top: t,
                    bottom: b,
                    body_line: Some(line),
                });
            }
            ParticipantKind::Table | ParticipantKind::OtherLine => {
                if is_valid_bbox(p) {
                    fw_elems.push(FwElem {
                        id,
                        top: p.top,
                        bottom: p.bottom,
                        body_line: None,
                    });
                }
            }
        }
    }

    let fw_body_lines = fw_elems.iter().filter(|e| e.body_line.is_some()).count();
    if region_lines > 0 {
        let ratio = fw_body_lines as f64 / region_lines as f64;
        if ratio > FULLWIDTH_LINE_RATIO_MAX {
            return None;
        }
    }
    if fw_elems.is_empty() {
        return None;
    }

    fw_elems.sort_by(|a, b| {
        a.top
            .total_cmp(&b.top)
            .then_with(|| a.bottom.total_cmp(&b.bottom))
            .then_with(|| a.id.cmp(&b.id))
    });
    fw_elems.dedup_by(|a, b| a.id == b.id);

    // 所属判定用マークを行一意化後の集合に合わせて再構築
    fw_mark.fill(false);
    for e in &fw_elems {
        fw_mark[e.id] = true;
        if let Some(line) = e.body_line {
            if line < line_words.len() {
                for &wid in &line_words[line] {
                    fw_mark[wid] = true;
                }
            }
        }
    }

    let blocks = link_fullwidth_blocks(parts, ids, &fw_elems, &fw_mark);
    let y_valleys = valleys_from_intervals(&project_intervals(parts, ids, false));

    let mut best: Option<Valley> = None;
    for v in y_valleys {
        if !is_adjacent_to_any_block(parts, ids, v, &blocks, &fw_mark) {
            continue;
        }
        match best {
            None => best = Some(v),
            Some(b) => {
                if v.start.total_cmp(&b.start) == std::cmp::Ordering::Less {
                    best = Some(v);
                }
            }
        }
    }
    best
}

fn link_fullwidth_blocks(
    parts: &[Participant],
    ids: &[usize],
    elems: &[FwElem],
    fw_mark: &[bool],
) -> Vec<FwBlock> {
    if elems.is_empty() {
        return Vec::new();
    }
    let mut blocks = Vec::new();
    let mut cur_top = elems[0].top;
    let mut cur_bot = elems[0].bottom;
    for b in elems.iter().skip(1) {
        // 現在ブロック区間との重なり・間隙で連結判定
        if b.top <= cur_bot {
            cur_top = cur_top.min(b.top);
            cur_bot = cur_bot.max(b.bottom);
        } else if !non_fw_y_intersects(parts, ids, fw_mark, cur_bot, b.top) {
            cur_top = cur_top.min(b.top);
            cur_bot = cur_bot.max(b.bottom);
        } else {
            blocks.push(FwBlock {
                top: cur_top,
                bottom: cur_bot,
            });
            cur_top = b.top;
            cur_bot = b.bottom;
        }
    }
    blocks.push(FwBlock {
        top: cur_top,
        bottom: cur_bot,
    });
    blocks
}

/// 開区間 (y0, y1) に全幅以外の y 投影が交差するか
fn non_fw_y_intersects(
    parts: &[Participant],
    ids: &[usize],
    fw_mark: &[bool],
    y0: f64,
    y1: f64,
) -> bool {
    if y1.total_cmp(&y0) != std::cmp::Ordering::Greater {
        return false;
    }
    for &id in ids {
        if fw_mark.get(id).copied().unwrap_or(false) {
            continue;
        }
        let p = &parts[id];
        if !is_valid_bbox(p) {
            continue;
        }
        if p.top < y1 && p.bottom > y0 {
            return true;
        }
    }
    false
}

fn is_adjacent_to_any_block(
    parts: &[Participant],
    ids: &[usize],
    v: Valley,
    blocks: &[FwBlock],
    fw_mark: &[bool],
) -> bool {
    for blk in blocks {
        if valley_adjacent_before(parts, ids, v, blk, fw_mark) {
            return true;
        }
        if valley_adjacent_after(parts, ids, v, blk, fw_mark) {
            return true;
        }
    }
    false
}

fn valley_adjacent_before(
    parts: &[Participant],
    ids: &[usize],
    v: Valley,
    blk: &FwBlock,
    fw_mark: &[bool],
) -> bool {
    // valley がブロックより上にあり、間に他投影が無い
    if v.end.total_cmp(&blk.top) == std::cmp::Ordering::Greater {
        return false;
    }
    if v.start.total_cmp(&blk.top) != std::cmp::Ordering::Less {
        return false;
    }
    !non_fw_y_intersects(parts, ids, fw_mark, v.end, blk.top)
}

fn valley_adjacent_after(
    parts: &[Participant],
    ids: &[usize],
    v: Valley,
    blk: &FwBlock,
    fw_mark: &[bool],
) -> bool {
    if v.start.total_cmp(&blk.bottom) == std::cmp::Ordering::Less {
        return false;
    }
    if v.end.total_cmp(&blk.bottom) != std::cmp::Ordering::Greater {
        return false;
    }
    !non_fw_y_intersects(parts, ids, fw_mark, blk.bottom, v.start)
}

fn build_tree(parts: &[Participant], ids: &[usize], rect: Rect, depth: usize) -> Tree {
    let valid_count = ids.iter().filter(|&&id| is_valid_bbox(&parts[id])).count();
    if depth >= MAX_DEPTH || valid_count <= 1 {
        return Tree::Leaf {
            parts: ids.to_vec(),
        };
    }
    if let Some(v) = try_x_split(parts, ids) {
        let (lo, hi) = split_ids(parts, ids, v, true);
        let mid = v.mid();
        let left_rect = Rect {
            left: rect.left,
            right: mid,
            top: rect.top,
            bottom: rect.bottom,
        };
        let right_rect = Rect {
            left: mid,
            right: rect.right,
            top: rect.top,
            bottom: rect.bottom,
        };
        return Tree::Split {
            is_x: true,
            left: Box::new(build_tree(parts, &lo, left_rect, depth + 1)),
            right: Box::new(build_tree(parts, &hi, right_rect, depth + 1)),
        };
    }
    if let Some(v) = try_y_split(parts, ids) {
        let (lo, hi) = split_ids(parts, ids, v, false);
        let mid = v.mid();
        let top_rect = Rect {
            left: rect.left,
            right: rect.right,
            top: rect.top,
            bottom: mid,
        };
        let bot_rect = Rect {
            left: rect.left,
            right: rect.right,
            top: mid,
            bottom: rect.bottom,
        };
        return Tree::Split {
            is_x: false,
            left: Box::new(build_tree(parts, &lo, top_rect, depth + 1)),
            right: Box::new(build_tree(parts, &hi, bot_rect, depth + 1)),
        };
    }
    Tree::Leaf {
        parts: ids.to_vec(),
    }
}

fn tree_has_x(t: &Tree) -> bool {
    match t {
        Tree::Leaf { .. } => false,
        Tree::Split { is_x: true, .. } => true,
        Tree::Split {
            is_x: false,
            left,
            right,
        } => tree_has_x(left) || tree_has_x(right),
    }
}

fn fold_tree(t: Tree) -> Tree {
    match t {
        Tree::Leaf { parts } => Tree::Leaf { parts },
        Tree::Split { is_x, left, right } => {
            let left = fold_tree(*left);
            let right = fold_tree(*right);
            if !is_x && !tree_has_x(&left) && !tree_has_x(&right) {
                let mut parts = collect_parts(&left);
                parts.extend(collect_parts(&right));
                parts.sort_unstable();
                Tree::Leaf { parts }
            } else {
                Tree::Split {
                    is_x,
                    left: Box::new(left),
                    right: Box::new(right),
                }
            }
        }
    }
}

fn collect_parts(t: &Tree) -> Vec<usize> {
    match t {
        Tree::Leaf { parts } => parts.clone(),
        Tree::Split { left, right, .. } => {
            let mut v = collect_parts(left);
            v.extend(collect_parts(right));
            v
        }
    }
}

fn assign_leaves(t: &Tree, leaf_of: &mut [usize], next: &mut usize) {
    match t {
        Tree::Leaf { parts } => {
            let id = *next;
            *next += 1;
            for &p in parts {
                leaf_of[p] = id;
            }
        }
        Tree::Split { left, right, .. } => {
            assign_leaves(left, leaf_of, next);
            assign_leaves(right, leaf_of, next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(line: usize, left: f64, top: f64, right: f64, bottom: f64) -> Participant {
        Participant {
            left,
            right,
            top,
            bottom,
            kind: ParticipantKind::Word { line },
        }
    }

    fn table(left: f64, top: f64, right: f64, bottom: f64) -> Participant {
        Participant {
            left,
            right,
            top,
            bottom,
            kind: ParticipantKind::Table,
        }
    }

    fn other_line(left: f64, top: f64, right: f64, bottom: f64) -> Participant {
        Participant {
            left,
            right,
            top,
            bottom,
            kind: ParticipantKind::OtherLine,
        }
    }

    /// 2列の本文単語を生成する（各列 n 行、1行1単語。左列→右列の順）
    fn two_col_words(
        n: usize,
        left_x0: f64,
        left_x1: f64,
        right_x0: f64,
        right_x1: f64,
        y0: f64,
        row_h: f64,
        gap: f64,
    ) -> Vec<Participant> {
        let mut parts = Vec::new();
        for i in 0..n {
            let top = y0 + i as f64 * (row_h + gap);
            let bot = top + row_h;
            parts.push(word(i, left_x0, top, left_x1, bot));
        }
        for i in 0..n {
            let top = y0 + i as f64 * (row_h + gap);
            let bot = top + row_h;
            parts.push(word(n + i, right_x0, top, right_x1, bot));
        }
        parts
    }

    #[test]
    fn two_columns_left_then_right() {
        let parts = two_col_words(5, 20.0, 80.0, 100.0, 160.0, 20.0, 12.0, 8.0);
        let leaves = split_regions(200.0, 300.0, &parts).expect("x-split");
        for i in 0..5 {
            assert_eq!(leaves[i], 0, "left col line {i}");
        }
        for i in 5..10 {
            assert_eq!(leaves[i], 1, "right col line {}", i - 5);
        }
    }

    #[test]
    fn x_valley_width_boundary() {
        let parts = two_col_words(5, 20.0, 80.0, 90.0, 150.0, 20.0, 12.0, 8.0);
        assert!(split_regions(200.0, 300.0, &parts).is_some());
        let parts = two_col_words(5, 20.0, 80.0, 89.0, 149.0, 20.0, 12.0, 8.0);
        assert!(split_regions(200.0, 300.0, &parts).is_none());
    }

    #[test]
    fn x_side_line_count_boundary() {
        let parts = two_col_words(5, 20.0, 80.0, 100.0, 160.0, 20.0, 12.0, 8.0);
        assert!(split_regions(200.0, 300.0, &parts).is_some());
        let parts = two_col_words(4, 20.0, 80.0, 100.0, 160.0, 20.0, 12.0, 8.0);
        assert!(split_regions(200.0, 300.0, &parts).is_none());
    }

    #[test]
    fn x_density_boundary() {
        // 低密度 → 不成立
        let mut parts = Vec::new();
        for i in 0..5 {
            let top = 20.0 + i as f64 * 50.0;
            parts.push(word(i, 20.0, top, 80.0, top + 2.0));
        }
        for i in 0..5 {
            let top = 20.0 + i as f64 * 50.0;
            parts.push(word(5 + i, 100.0, top, 160.0, top + 2.0));
        }
        assert!(split_regions(200.0, 400.0, &parts).is_none());

        // 密度ちょうど 0.3
        let mut parts = Vec::new();
        for i in 0..5 {
            let top = 20.0 + i as f64 * 47.0;
            parts.push(word(i, 20.0, top, 80.0, top + 12.0));
        }
        for i in 0..5 {
            let top = 20.0 + i as f64 * 47.0;
            parts.push(word(5 + i, 100.0, top, 160.0, top + 12.0));
        }
        assert!(split_regions(200.0, 400.0, &parts).is_some());
    }

    #[test]
    fn x_width_ratio_boundary() {
        let mut parts = Vec::new();
        for i in 0..5 {
            let top = 20.0 + i as f64 * 20.0;
            parts.push(word(i, 20.0, top, 40.0, top + 12.0));
        }
        for i in 0..5 {
            let top = 20.0 + i as f64 * 20.0;
            parts.push(word(5 + i, 60.0, top, 160.0, top + 12.0));
        }
        assert!(split_regions(200.0, 300.0, &parts).is_some());

        let mut parts = Vec::new();
        for i in 0..5 {
            let top = 20.0 + i as f64 * 20.0;
            parts.push(word(i, 20.0, top, 39.0, top + 12.0));
        }
        for i in 0..5 {
            let top = 20.0 + i as f64 * 20.0;
            parts.push(word(5 + i, 60.0, top, 160.0, top + 12.0));
        }
        assert!(split_regions(200.0, 300.0, &parts).is_none());
    }

    #[test]
    fn x_split_priority_over_aligned_paragraph_gaps() {
        let mut parts = Vec::new();
        for i in 0..3 {
            let top = 20.0 + i as f64 * 16.0;
            parts.push(word(i, 20.0, top, 80.0, top + 12.0));
        }
        for i in 0..2 {
            let top = 120.0 + i as f64 * 16.0;
            parts.push(word(3 + i, 20.0, top, 80.0, top + 12.0));
        }
        for i in 0..3 {
            let top = 20.0 + i as f64 * 16.0;
            parts.push(word(10 + i, 100.0, top, 160.0, top + 12.0));
        }
        for i in 0..2 {
            let top = 120.0 + i as f64 * 16.0;
            parts.push(word(13 + i, 100.0, top, 160.0, top + 12.0));
        }
        let leaves = split_regions(200.0, 300.0, &parts).expect("x first");
        let max_leaf = leaves.iter().copied().max().unwrap();
        assert_eq!(max_leaf, 1);
        for i in 0..5 {
            assert_eq!(leaves[i], 0);
            assert_eq!(leaves[5 + i], 1);
        }
    }

    #[test]
    fn fullwidth_heading_then_two_columns() {
        let mut parts = Vec::new();
        parts.push(word(0, 20.0, 10.0, 180.0, 24.0));
        for i in 0..5 {
            let top = 40.0 + i as f64 * 20.0;
            parts.push(word(1 + i, 20.0, top, 80.0, top + 12.0));
        }
        for i in 0..5 {
            let top = 40.0 + i as f64 * 20.0;
            parts.push(word(6 + i, 100.0, top, 160.0, top + 12.0));
        }
        let leaves = split_regions(200.0, 300.0, &parts).expect("split");
        assert_eq!(leaves[0], 0);
        for i in 1..6 {
            assert_eq!(leaves[i], 1, "left {i}");
        }
        for i in 6..11 {
            assert_eq!(leaves[i], 2, "right {i}");
        }
    }

    #[test]
    fn two_columns_then_center_footer() {
        let mut parts = two_col_words(5, 20.0, 80.0, 100.0, 160.0, 20.0, 12.0, 8.0);
        parts.push(word(100, 70.0, 200.0, 130.0, 214.0));
        let leaves = split_regions(200.0, 300.0, &parts).expect("split");
        for i in 0..5 {
            assert_eq!(leaves[i], 0);
        }
        for i in 5..10 {
            assert_eq!(leaves[i], 1);
        }
        assert_eq!(leaves[10], 2);
    }

    #[test]
    fn fullwidth_block_links_multiline_heading_band() {
        // 全幅3行 + 2列5行 + 中央フッタ（全幅行比 4/14 ≤ 0.3）
        // 全幅帯が一括分離され、行間 valley で1行ずつ割れない
        let mut parts = Vec::new();
        parts.push(word(0, 20.0, 10.0, 180.0, 22.0));
        parts.push(word(1, 20.0, 24.0, 180.0, 36.0));
        parts.push(word(2, 20.0, 40.0, 180.0, 52.0));
        for i in 0..5 {
            let top = 70.0 + i as f64 * 18.0;
            parts.push(word(10 + i, 20.0, top, 80.0, top + 12.0));
        }
        for i in 0..5 {
            let top = 70.0 + i as f64 * 18.0;
            parts.push(word(20 + i, 100.0, top, 160.0, top + 12.0));
        }
        parts.push(word(30, 70.0, 190.0, 130.0, 202.0));
        let leaves = split_regions(200.0, 300.0, &parts).expect("split");
        // 全幅3行は同一葉
        let h0 = leaves[0];
        assert_eq!(leaves[1], h0);
        assert_eq!(leaves[2], h0);
        let left = leaves[3];
        let right = leaves[8];
        let footer = leaves[13];
        assert_eq!(h0, 0);
        assert_eq!(left, 1);
        assert_eq!(right, 2);
        assert_eq!(footer, 3);
        for i in 3..8 {
            assert_eq!(leaves[i], left);
        }
        for i in 8..13 {
            assert_eq!(leaves[i], right);
        }
    }

    #[test]
    fn fullwidth_table_between_column_bands() {
        let mut parts = Vec::new();
        for i in 0..5 {
            let top = 10.0 + i as f64 * 16.0;
            parts.push(word(i, 20.0, top, 80.0, top + 12.0));
        }
        for i in 0..5 {
            let top = 10.0 + i as f64 * 16.0;
            parts.push(word(10 + i, 100.0, top, 160.0, top + 12.0));
        }
        parts.push(table(20.0, 100.0, 180.0, 140.0));
        for i in 0..5 {
            let top = 160.0 + i as f64 * 16.0;
            parts.push(word(20 + i, 20.0, top, 80.0, top + 12.0));
        }
        for i in 0..5 {
            let top = 160.0 + i as f64 * 16.0;
            parts.push(word(30 + i, 100.0, top, 160.0, top + 12.0));
        }
        let leaves = split_regions(200.0, 300.0, &parts).expect("split");
        assert_eq!(leaves[0], 0);
        assert_eq!(leaves[5], 1);
        assert_eq!(leaves[10], 2);
        assert_eq!(leaves[11], 3);
        assert_eq!(leaves[16], 4);
        for i in 0..5 {
            assert_eq!(leaves[i], 0);
            assert_eq!(leaves[5 + i], 1);
            assert_eq!(leaves[11 + i], 3);
            assert_eq!(leaves[16 + i], 4);
        }
    }

    #[test]
    fn fullwidth_line_ratio_boundary_and_merge() {
        // 全行が跨ぐ → 行比超過 → None
        let mut parts = Vec::new();
        for i in 0..10 {
            let top = 20.0 + i as f64 * 18.0;
            parts.push(word(i, 20.0, top, 180.0, top + 12.0));
        }
        assert!(split_regions(200.0, 400.0, &parts).is_none());

        // 3/13 ≈ 0.23 ≤ 0.3 → Y 後に X が走り得る
        let mut parts = Vec::new();
        for i in 0..3 {
            let top = 10.0 + i as f64 * 14.0;
            parts.push(word(i, 20.0, top, 180.0, top + 12.0));
        }
        for i in 0..5 {
            let top = 60.0 + i as f64 * 18.0;
            parts.push(word(3 + i, 20.0, top, 80.0, top + 12.0));
        }
        for i in 0..5 {
            let top = 60.0 + i as f64 * 18.0;
            parts.push(word(8 + i, 100.0, top, 160.0, top + 12.0));
        }
        assert!(split_regions(200.0, 300.0, &parts).is_some());

        // 合併後に 0.3 超 → None
        let mut parts = Vec::new();
        for i in 0..8 {
            let top = 10.0 + i as f64 * 16.0;
            parts.push(word(i, 20.0, top, 180.0, top + 12.0));
        }
        for i in 0..2 {
            let top = 150.0 + i as f64 * 16.0;
            parts.push(word(10 + i, 20.0, top, 80.0, top + 12.0));
            parts.push(word(20 + i, 100.0, top, 160.0, top + 12.0));
        }
        assert!(split_regions(200.0, 300.0, &parts).is_none());
    }

    #[test]
    fn no_split_single_column_toc_empty_singleton() {
        let mut parts = Vec::new();
        for i in 0..8 {
            let top = 20.0 + i as f64 * 16.0;
            parts.push(word(i, 40.0, top, 160.0, top + 12.0));
        }
        assert!(split_regions(200.0, 300.0, &parts).is_none());

        let mut parts = Vec::new();
        for i in 0..6 {
            let top = 20.0 + i as f64 * 16.0;
            parts.push(word(i, 20.0, top, 140.0, top + 12.0));
            parts.push(word(10 + i, 160.0, top, 180.0, top + 12.0));
        }
        assert!(split_regions(200.0, 300.0, &parts).is_none());

        assert!(split_regions(200.0, 300.0, &[]).is_none());

        let parts = vec![word(0, 20.0, 20.0, 80.0, 32.0)];
        assert!(split_regions(200.0, 300.0, &parts).is_none());
    }

    #[test]
    fn degenerate_bbox_excluded_from_projection() {
        let mut parts = two_col_words(5, 20.0, 80.0, 100.0, 160.0, 20.0, 12.0, 8.0);
        parts.push(word(99, 50.0, 50.0, 50.0, 60.0));
        parts.push(word(100, f64::NAN, 10.0, 20.0, 20.0));
        let leaves = split_regions(200.0, 300.0, &parts).expect("split");
        assert_eq!(leaves.len(), parts.len());
        assert_eq!(leaves[0], 0);
        assert_eq!(leaves[5], 1);
        assert!(leaves[10] == 0 || leaves[10] == 1);
        assert!(leaves[11] == 0 || leaves[11] == 1);
    }

    #[test]
    fn depth_cutoff_and_fold_interaction() {
        let mut parts = Vec::new();
        for i in 0..5 {
            let top = 10.0 + i as f64 * 14.0;
            parts.push(word(i, 20.0, top, 80.0, top + 10.0));
        }
        for i in 0..5 {
            let top = 120.0 + i as f64 * 14.0;
            parts.push(word(5 + i, 20.0, top, 80.0, top + 10.0));
        }
        for i in 0..5 {
            let top = 10.0 + i as f64 * 14.0;
            parts.push(word(20 + i, 100.0, top, 160.0, top + 10.0));
        }
        for i in 0..5 {
            let top = 120.0 + i as f64 * 14.0;
            parts.push(word(30 + i, 100.0, top, 160.0, top + 10.0));
        }
        let leaves = split_regions(200.0, 300.0, &parts).expect("x-split");
        let max_leaf = leaves.iter().copied().max().unwrap();
        assert_eq!(max_leaf, 1);
        for i in 0..10 {
            assert_eq!(leaves[i], 0, "left {i}");
        }
        for i in 10..20 {
            assert_eq!(leaves[i], 1, "right {i}");
        }
    }

    #[test]
    fn other_line_participates_in_projection() {
        // 他グループ行が gutter を塞ぎ、全幅として分離された後に列分割する
        let mut parts = two_col_words(5, 20.0, 80.0, 100.0, 160.0, 40.0, 12.0, 8.0);
        parts.insert(0, other_line(20.0, 10.0, 180.0, 28.0));
        let leaves = split_regions(200.0, 300.0, &parts).expect("split");
        assert_eq!(leaves[0], 0);
        for i in 1..6 {
            assert_eq!(leaves[i], 1);
        }
        for i in 6..11 {
            assert_eq!(leaves[i], 2);
        }
    }

    #[test]
    fn fullwidth_candidate_line_ratio_exactly_point_three() {
        // 3/10=0.3 ちょうど採用（左右は同一行IDの両側単語で5行以上）
        // 4/10>0.3 で棄却
        let mut parts = Vec::new();
        for i in 0..3 {
            let top = 6.0 + i as f64 * 12.0;
            parts.push(word(100 + i, 20.0, top, 180.0, top + 10.0));
        }
        for i in 0..7 {
            let top = 55.0 + i as f64 * 16.0;
            parts.push(word(i, 20.0, top, 80.0, top + 12.0));
            parts.push(word(i, 100.0, top, 160.0, top + 12.0));
        }
        // 本文行 3+7=10、全幅 3/10=0.3
        assert!(split_regions(200.0, 300.0, &parts).is_some());

        let mut parts = Vec::new();
        for i in 0..4 {
            let top = 6.0 + i as f64 * 11.0;
            parts.push(word(100 + i, 20.0, top, 180.0, top + 9.0));
        }
        for i in 0..6 {
            let top = 60.0 + i as f64 * 16.0;
            parts.push(word(i, 20.0, top, 80.0, top + 12.0));
            parts.push(word(i, 100.0, top, 160.0, top + 12.0));
        }
        // 4+6=10、全幅 4/10=0.4 > 0.3
        assert!(split_regions(200.0, 300.0, &parts).is_none());
    }

    #[test]
    fn fullwidth_merge_exceeds_after_per_candidate_ok() {
        // 別帯の行集合が各 ≤0.3、合併で 0.3 超 → Y 不成立
        // 本文10行: 帯Aを塞ぐ3行・帯Bを塞ぐ3行（各0.3）、両側列2行ずつ
        let mut parts = Vec::new();
        for i in 0..3 {
            let top = 10.0 + i as f64 * 14.0;
            parts.push(word(i, 70.0, top, 100.0, top + 12.0));
        }
        for i in 0..3 {
            let top = 80.0 + i as f64 * 14.0;
            parts.push(word(3 + i, 90.0, top, 130.0, top + 12.0));
        }
        for i in 0..2 {
            let top = 150.0 + i as f64 * 16.0;
            parts.push(word(10 + i, 20.0, top, 60.0, top + 12.0));
            parts.push(word(20 + i, 140.0, top, 180.0, top + 12.0));
        }
        assert!(split_regions(200.0, 300.0, &parts).is_none());
    }

    #[test]
    fn us001_style_three_col_with_staggered_fullwidth_notes() {
        // 3列 + 全幅表 + キャプション + 互いにずれた全幅脚注行
        // 脚注は密な複数単語で段間を部分塞ぎし、行昇格除去でのみ ≥10pt 空帯が開く
        let mut parts = Vec::new();
        parts.push(table(20.0, 5.0, 250.0, 40.0));
        parts.push(word(0, 20.0, 44.0, 250.0, 56.0));
        // 3列（各5行）L[20,70] M[110,160] R[200,250]
        for i in 0..5 {
            let top = 70.0 + i as f64 * 18.0;
            parts.push(word(10 + i, 20.0, top, 70.0, top + 12.0));
            parts.push(word(20 + i, 110.0, top, 160.0, top + 12.0));
            parts.push(word(30 + i, 200.0, top, 250.0, top + 12.0));
        }
        // 脚注1: 単語間隙 <10pt でページを横断（単語単位除去では ≥10pt valley 不成立）
        let fn1 = 100usize;
        for &(l, r) in &[
            (20.0, 48.0),
            (50.0, 78.0),
            (80.0, 108.0),
            (110.0, 138.0),
            (140.0, 168.0),
            (170.0, 198.0),
            (200.0, 228.0),
            (230.0, 250.0),
        ] {
            parts.push(word(fn1, l, 175.0, r, 187.0));
        }
        // 脚注2: 同様にずれた分割
        let fn2 = 101usize;
        for &(l, r) in &[
            (20.0, 45.0),
            (48.0, 75.0),
            (78.0, 105.0),
            (108.0, 135.0),
            (138.0, 165.0),
            (168.0, 195.0),
            (198.0, 225.0),
            (228.0, 250.0),
        ] {
            parts.push(word(fn2, l, 195.0, r, 207.0));
        }

        let leaves = split_regions(270.0, 320.0, &parts).expect("us001-style split");
        // parts 順: table, cap, 各行 (L,M,R)×5, 脚注単語群
        // 列本文は index 2 + 3*i + {0,1,2}
        let left_leaf = leaves[2];
        let mid_leaf = leaves[3];
        let right_leaf = leaves[4];
        assert_ne!(left_leaf, mid_leaf);
        assert_ne!(mid_leaf, right_leaf);
        assert_ne!(left_leaf, right_leaf);
        for i in 0..5 {
            assert_eq!(leaves[2 + 3 * i], left_leaf, "left row {i}");
            assert_eq!(leaves[3 + 3 * i], mid_leaf, "mid row {i}");
            assert_eq!(leaves[4 + 3 * i], right_leaf, "right row {i}");
        }
        assert!(left_leaf < mid_leaf);
        assert!(mid_leaf < right_leaf);
        assert!(leaves[0] < left_leaf);
        // 脚注は列と別葉（下帯）
        let fn_leaf = leaves[17];
        assert!(fn_leaf > right_leaf || fn_leaf != left_leaf);
        for i in 17..leaves.len() {
            assert_eq!(leaves[i], fn_leaf);
        }
    }

    #[test]
    fn depth_eight_region_is_leaf() {
        // Y 分割を 8 段重ねた末帯に 2 列を置き、深さ 8 で葉になること
        // （深さ 8 では X 分割せず両列が同一葉）
        let mut parts = Vec::new();
        // 先行する X 分割（畳み込み回避）: 浅い位置の 2 列
        for i in 0..5 {
            let top = 5.0 + i as f64 * 12.0;
            parts.push(word(i, 20.0, top, 80.0, top + 10.0));
            parts.push(word(10 + i, 100.0, top, 160.0, top + 10.0));
        }
        // 深さ稼ぎ: 左列側に全幅相当の帯を Y で多段分離させるため
        // ページ全体を横切るセパレータを縦に 8 本
        for s in 0..8 {
            let top = 80.0 + s as f64 * 22.0;
            parts.push(word(100 + s, 20.0, top, 180.0, top + 10.0));
            // 各帯に孤立単語（分割継続用）
            parts.push(word(200 + s, 30.0, top + 12.0, 50.0, top + 18.0));
        }
        // 最下帯に 2 列 5 行（深さ上限で割れない想定）
        for i in 0..5 {
            let top = 80.0 + 8.0 * 22.0 + 20.0 + i as f64 * 14.0;
            parts.push(word(300 + i, 20.0, top, 80.0, top + 10.0));
            parts.push(word(400 + i, 100.0, top, 160.0, top + 10.0));
        }
        let leaves = split_regions(200.0, 500.0, &parts).expect("has x");
        // 最下 2 列の単語が同一葉なら深さ打ち切りで X 未実施
        let n = parts.len();
        let bottom_left_start = n - 10;
        let leaf_a = leaves[bottom_left_start];
        let leaf_b = leaves[bottom_left_start + 5];
        // 深さ 8 到達なら同一葉。到達していなければ別葉（テスト失敗で検知）
        assert_eq!(
            leaf_a, leaf_b,
            "depth-8 band should keep both columns in one leaf"
        );
        for i in 0..5 {
            assert_eq!(leaves[bottom_left_start + i], leaf_a);
            assert_eq!(leaves[bottom_left_start + 5 + i], leaf_a);
        }
    }

    #[test]
    fn atomic_band_cross_tolerance_ignores_sub_half_pt_protrusion() {
        // 2列 + 段間を互いにずれた全幅行で塞ぐ + 段間縁へ ≤0.5pt はみ出す本文行
        // はみ出し行は全幅にならず列の一部として分割される
        // 対照: 大きくはみ出す行は従来どおり F に入り、除去後に X が成立する
        let mut parts = Vec::new();
        // 全幅見出し（ずれた帯を塞ぐ）
        parts.push(word(0, 20.0, 8.0, 180.0, 20.0));
        parts.push(word(1, 30.0, 24.0, 170.0, 36.0));
        // 左列5行 [20, 80]、うち1行だけ 0.4pt はみ出し
        for i in 0..5 {
            let top = 50.0 + i as f64 * 18.0;
            let right = if i == 2 { 80.4 } else { 80.0 };
            parts.push(word(10 + i, 20.0, top, right, top + 12.0));
        }
        // 右列5行 [100, 160]
        for i in 0..5 {
            let top = 50.0 + i as f64 * 18.0;
            parts.push(word(20 + i, 100.0, top, 160.0, top + 12.0));
        }
        // 下部のずれた全幅行（別帯）
        parts.push(word(30, 25.0, 160.0, 175.0, 172.0));
        parts.push(word(31, 40.0, 176.0, 160.0, 188.0));

        let leaves = split_regions(200.0, 300.0, &parts).expect("split with tolerance");
        // parts: fw0, fw1, L0..L4, R0..R4, fw2, fw3
        let left = leaves[2];
        let right = leaves[7];
        assert_ne!(left, right);
        for i in 0..5 {
            assert_eq!(leaves[2 + i], left, "left row {i} stays in column");
            assert_eq!(leaves[7 + i], right, "right row {i} stays in column");
        }
        // はみ出し行（L2 = index 4）も他の左列と同じ葉
        assert_eq!(leaves[4], left);
        // 全幅見出しは列より上の葉
        assert!(leaves[0] < left);
        assert!(leaves[1] < left || leaves[1] == leaves[0]);

        // 対照: 最終行だけ 15pt はみ出し（残 valley 5pt < 10）
        // F に入らないと X 不成立。10行あれば最終行分離後も上側で X 成立
        let mut parts = Vec::new();
        parts.push(word(0, 20.0, 8.0, 180.0, 20.0));
        parts.push(word(1, 30.0, 24.0, 170.0, 36.0));
        for i in 0..10 {
            let top = 50.0 + i as f64 * 18.0;
            let right = if i == 9 { 95.0 } else { 80.0 };
            parts.push(word(10 + i, 20.0, top, right, top + 12.0));
        }
        for i in 0..10 {
            let top = 50.0 + i as f64 * 18.0;
            parts.push(word(30 + i, 100.0, top, 160.0, top + 12.0));
        }

        let leaves = split_regions(200.0, 400.0, &parts).expect("large protrusion enters F");
        // parts: fw0, fw1, L0..L9, R0..R9
        // はみ出しは L9 = index 11
        let protrude = leaves[11];
        let left0 = leaves[2];
        assert_ne!(protrude, left0, "large protrusion should leave the column leaf");
        // 上側左列は同一葉（最終行以外）
        for i in 0..9 {
            assert_eq!(leaves[2 + i], left0, "left row {i}");
        }
        let right0 = leaves[12];
        assert_ne!(left0, right0);
        for i in 0..9 {
            assert_eq!(leaves[12 + i], right0, "right row {i}");
        }
    }
}
