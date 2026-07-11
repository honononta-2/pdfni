//! ヘッダ・フッタ検出（文書単位の後段パス）

use crate::document::{
    DocBlock, DocDoc, TextBlockRole, block_rep_font_size, sanitize_md_text,
};

/// 隣接制約の縁距離上限（pt）
const EDGE_GAP_MAX: f64 = 30.0;
/// font_size 近似の比上限
const FS_RATIO_MAX: f64 = 1.05;

/// 文書単位でヘッダ・フッタの役割を付ける
pub fn assign_header_footer(doc: &mut DocDoc) {
    for page in &mut doc.pages {
        for block in &mut page.blocks {
            if let DocBlock::Text(tb) = block {
                tb.role = TextBlockRole::Body;
            }
        }
    }

    if doc.pages.len() < 2 {
        return;
    }

    detect_direction(doc, Direction::Header);
    detect_direction(doc, Direction::Footer);
}

#[derive(Clone, Copy)]
enum Direction {
    Header,
    Footer,
}

impl Direction {
    fn role(self) -> TextBlockRole {
        match self {
            Direction::Header => TextBlockRole::Header,
            Direction::Footer => TextBlockRole::Footer,
        }
    }
}

/// 1ページ分の候補
struct Cand {
    block_idx: usize,
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    text: String,
    font_size: Option<f64>,
}

fn detect_direction(doc: &mut DocDoc, dir: Direction) {
    let n_pages = doc.pages.len();
    let mut candidates: Vec<Vec<Cand>> = Vec::with_capacity(n_pages);
    for page in &doc.pages {
        candidates.push(page_candidates(page, dir));
    }

    let mut n_p = vec![0usize; n_pages];

    let mut k = 0usize;
    loop {
        // 今ラウンドの比較対象があるか
        let mut has_target = vec![false; n_pages];
        for p in 0..n_pages {
            if n_p[p] != k {
                continue;
            }
            if candidates[p].len() < k + 1 {
                continue;
            }
            if k > 0 {
                let prev = &candidates[p][k - 1];
                let cur = &candidates[p][k];
                let gap = match dir {
                    Direction::Header => cur.top - prev.bottom,
                    Direction::Footer => prev.top - cur.bottom,
                };
                if gap > EDGE_GAP_MAX {
                    continue;
                }
            }
            has_target[p] = true;
        }

        // ページ対の成立
        let mut pair_ok = vec![false; n_pages];
        for d in [1usize, 2usize] {
            if n_pages <= d {
                continue;
            }
            for p in 0..n_pages - d {
                if !has_target[p] || !has_target[p + d] {
                    continue;
                }
                let a = &candidates[p][k];
                let b = &candidates[p + d][k];
                if blocks_match(a, b, d as u64) {
                    pair_ok[p] = true;
                    pair_ok[p + d] = true;
                }
            }
        }

        // §3-5: ページ添字順に一括更新
        for p in 0..n_pages {
            if pair_ok[p] {
                n_p[p] = k + 1;
            }
        }

        // §3-6: 鏡映の救済（pair_ok のみを両隣の成立と数える）
        let mut any_success = pair_ok.iter().any(|&x| x);
        for p in 0..n_pages {
            if n_p[p] != k {
                continue;
            }
            if !has_target[p] {
                continue;
            }
            if p == 0 || p + 1 >= n_pages {
                continue;
            }
            if !(pair_ok[p - 1] && pair_ok[p + 1]) {
                continue;
            }
            // 両隣は §3-5 で n = k+1 になっている
            if n_p[p - 1] != k + 1 || n_p[p + 1] != k + 1 {
                continue;
            }
            let c = &candidates[p][k];
            let left = &candidates[p - 1][k];
            let right = &candidates[p + 1][k];
            if mirror_rescue_match(left, c, right) {
                n_p[p] = k + 1;
                any_success = true;
            }
        }

        if !any_success {
            break;
        }
        k += 1;
    }

    // 役割を付与
    let role = dir.role();
    for (p, page) in doc.pages.iter_mut().enumerate() {
        let count = n_p[p];
        for c in candidates[p].iter().take(count) {
            if let DocBlock::Text(tb) = &mut page.blocks[c.block_idx] {
                tb.role = role;
            }
        }
    }
}

fn page_candidates(page: &crate::document::DocPage, dir: Direction) -> Vec<Cand> {
    let w = page.width;
    let h = page.height;
    if !(w.is_finite() && w > 0.0 && h.is_finite() && h > 0.0) {
        return Vec::new();
    }

    let mut cands = Vec::new();
    for (bi, block) in page.blocks.iter().enumerate() {
        let DocBlock::Text(tb) = block else {
            continue;
        };
        if !is_candidate_text(&tb.text) {
            continue;
        }
        let in_band = match dir {
            Direction::Header => tb.bottom <= h / 6.0,
            Direction::Footer => tb.top >= h * 5.0 / 6.0,
        };
        if !in_band {
            continue;
        }
        cands.push(Cand {
            block_idx: bi,
            left: tb.left,
            right: tb.right,
            top: tb.top,
            bottom: tb.bottom,
            text: tb.text.clone(),
            font_size: block_rep_font_size(&page.chars, tb),
        });
    }

    match dir {
        Direction::Header => {
            cands.sort_by(|a, b| {
                a.top
                    .total_cmp(&b.top)
                    .then_with(|| a.block_idx.cmp(&b.block_idx))
            });
        }
        Direction::Footer => {
            cands.sort_by(|a, b| {
                b.bottom
                    .total_cmp(&a.bottom)
                    .then_with(|| a.block_idx.cmp(&b.block_idx))
            });
        }
    }
    cands
}

/// Markdown と同じ空判定で候補になるか
fn is_candidate_text(text: &str) -> bool {
    !sanitize_md_text(text).trim().is_empty()
}

fn blocks_match(a: &Cand, b: &Cand, d: u64) -> bool {
    bbox_intersects(a, b) && font_size_close(a.font_size, b.font_size) && texts_match(&a.text, &b.text, d)
}

fn bbox_intersects(a: &Cand, b: &Cand) -> bool {
    a.left.max(b.left) < a.right.min(b.right) && a.top.max(b.top) < a.bottom.min(b.bottom)
}

fn font_size_close(a: Option<f64>, b: Option<f64>) -> bool {
    let (Some(fa), Some(fb)) = (a, b) else {
        return false;
    };
    let lo = fa.min(fb);
    let hi = fa.max(fb);
    if lo <= 0.0 {
        return false;
    }
    hi / lo <= FS_RATIO_MAX
}

fn texts_match(a: &str, b: &str, d: u64) -> bool {
    if a == b {
        return true;
    }
    let da = decompose(a);
    let db = decompose(b);
    if da.non_digits != db.non_digits {
        return false;
    }
    if da.digit_runs.len() != db.digit_runs.len() {
        return false;
    }
    for (ra, rb) in da.digit_runs.iter().zip(db.digit_runs.iter()) {
        let value_ok = match (ra.value, rb.value) {
            (Some(va), Some(vb)) => vb == va || vb == va + d,
            _ => false,
        };
        if value_ok || ra.text == rb.text {
            continue;
        }
        return false;
    }
    true
}

/// 鏡映救済: c と両隣で bbox・font・鏡映一致と値差 +1
fn mirror_rescue_match(left: &Cand, c: &Cand, right: &Cand) -> bool {
    if !bbox_intersects(c, left) || !bbox_intersects(c, right) {
        return false;
    }
    if !font_size_close(c.font_size, left.font_size)
        || !font_size_close(c.font_size, right.font_size)
    {
        return false;
    }
    let Some((vl, vc1)) = mirror_pair_values(&left.text, &c.text) else {
        return false;
    };
    let Some((vc2, vr)) = mirror_pair_values(&c.text, &right.text) else {
        return false;
    };
    if vc1 != vc2 {
        return false;
    }
    vl + 1 == vc1 && vc1 + 1 == vr
}

/// 両ブロックが鏡映一致なら (先の値, 後の値)
fn mirror_pair_values(a: &str, b: &str) -> Option<(u64, u64)> {
    let da = decompose(a);
    let db = decompose(b);
    if da.digit_runs.len() != 1 || db.digit_runs.len() != 1 {
        return None;
    }
    let va = da.digit_runs[0].value?;
    let vb = db.digit_runs[0].value?;
    if residual(&da) != residual(&db) {
        return None;
    }
    Some((va, vb))
}

struct DigitRun {
    text: String,
    value: Option<u64>,
}

struct Decomp {
    non_digits: Vec<String>,
    digit_runs: Vec<DigitRun>,
}

fn is_digit_char(c: char) -> bool {
    matches!(c, '\u{0030}'..='\u{0039}' | '\u{FF10}'..='\u{FF19}')
}

fn digit_to_u32(c: char) -> u32 {
    match c {
        '0'..='9' => c as u32 - '0' as u32,
        '\u{FF10}'..='\u{FF19}' => c as u32 - 0xFF10,
        _ => 0,
    }
}

fn decompose(text: &str) -> Decomp {
    let mut non_digits = Vec::new();
    let mut digit_runs = Vec::new();
    let mut cur_non = String::new();
    let mut cur_dig = String::new();
    let mut in_digit = false;

    for c in text.chars() {
        if is_digit_char(c) {
            if !in_digit {
                non_digits.push(std::mem::take(&mut cur_non));
                in_digit = true;
            }
            cur_dig.push(c);
        } else {
            if in_digit {
                digit_runs.push(make_digit_run(std::mem::take(&mut cur_dig)));
                in_digit = false;
            }
            cur_non.push(c);
        }
    }
    if in_digit {
        digit_runs.push(make_digit_run(cur_dig));
        non_digits.push(String::new());
    } else {
        non_digits.push(cur_non);
    }

    Decomp {
        non_digits,
        digit_runs,
    }
}

fn make_digit_run(text: String) -> DigitRun {
    let n = text.chars().count();
    let value = if n >= 19 {
        None
    } else {
        let mut v = 0u64;
        for c in text.chars() {
            v = v * 10 + u64::from(digit_to_u32(c));
        }
        Some(v)
    };
    DigitRun { text, value }
}

fn residual(d: &Decomp) -> String {
    let joined: String = d.non_digits.iter().cloned().collect();
    let mut out = String::with_capacity(joined.len());
    let mut prev_space = false;
    for c in joined.chars() {
        if c == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    // 先頭・末尾の U+0020 のみ除去
    let bytes = out.as_bytes();
    let mut start = 0;
    let mut end = bytes.len();
    while start < end && bytes[start] == b' ' {
        start += 1;
    }
    while end > start && bytes[end - 1] == b' ' {
        end -= 1;
    }
    out[start..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate_block(text: &str, top: f64, bottom: f64) -> DocBlock {
        DocBlock::Text(crate::document::TextBlock {
            kind: crate::document::TextBlockKind::Paragraph,
            role: TextBlockRole::Body,
            text: text.into(),
            left: 10.0,
            right: 50.0,
            top,
            bottom,
            dir: "ltr".into(),
            rot: 0,
            lines: vec![],
        })
    }

    fn candidate_page(width: f64, height: f64) -> crate::document::DocPage {
        crate::document::DocPage {
            page_number: 1,
            width,
            height,
            fonts: vec![],
            chars: vec![],
            blocks: vec![
                candidate_block("header", 0.0, 10.0),
                candidate_block("footer", 590.0, 600.0),
            ],
        }
    }

    #[test]
    fn decompose_value_diff_zero_and_d() {
        assert!(texts_match("Page 1", "Page 1", 1));
        assert!(texts_match("Page 1", "Page 2", 1));
        assert!(!texts_match("Page 1", "Page 2", 0));
        // 逆方向（後が先より小さい）は値条件を満たさない
        assert!(!texts_match("Page 2", "Page 1", 1));
    }

    #[test]
    fn decompose_digit_count_mismatch() {
        assert!(!texts_match("Page 1 of 2", "Page 12", 1));
    }

    #[test]
    fn digit_run_19_digits_no_value() {
        let d18 = "1".repeat(18);
        let d19 = "1".repeat(19);
        let r18 = make_digit_run(d18.clone());
        let r19 = make_digit_run(d19.clone());
        assert_eq!(r18.value, Some(111_111_111_111_111_111));
        assert_eq!(r19.value, None);
        // 片側のみ19桁: 値条件は不可、文字列一致のみ
        assert!(!texts_match(&d19, &("1".repeat(18) + "2"), 1));
        assert!(texts_match(&d19, &d19, 1));
        // 両側19桁で文字列不一致
        let d19b = "2".repeat(19);
        assert!(!texts_match(&d19, &d19b, 1));
        // 18桁最大値 + d
        let a = "999999999999999999";
        let b = "1000000000000000000"; // 19桁 → 値なし。a は値あり
        assert!(!texts_match(a, b, 1));
        // 18桁の加算
        assert!(texts_match("1", "2", 1));
        let v = make_digit_run("999999999999999999".into());
        assert_eq!(v.value, Some(999_999_999_999_999_999));
        assert!(texts_match(
            "999999999999999998",
            "999999999999999999",
            1
        ));
    }

    #[test]
    fn fullwidth_and_ascii_digits() {
        assert!(texts_match("Page 1", "Page １", 0)); // 完全一致ではないが骨格+値
        // "1" vs "１" は text 不一致だが値は同じ
        assert!(texts_match("1", "１", 0));
        assert!(texts_match("1", "２", 1));
        // 全角混在の1連
        assert_eq!(make_digit_run("1２3".into()).value, Some(123));
        assert!(texts_match("1２3", "124", 1));
    }

    #[test]
    fn digits_only_and_nul_in_text() {
        assert!(texts_match("42", "43", 1));
        // U+0000 を含む骨格
        let a = "a\u{0000}1";
        let b = "a\u{0000}2";
        assert!(texts_match(a, b, 1));
        let d = decompose(a);
        assert_eq!(d.non_digits, vec!["a\u{0000}".to_string(), String::new()]);
    }

    #[test]
    fn residual_space_compress() {
        let d = decompose("Page  1  of");
        // non: "Page  ", "  of" 連結 → "Page    of" → 圧縮 "Page of"
        assert_eq!(residual(&d), "Page of");
    }

    #[test]
    fn empty_and_whitespace_not_candidate() {
        assert!(!is_candidate_text(""));
        assert!(!is_candidate_text("   "));
        assert!(!is_candidate_text("\t\n"));
        assert!(is_candidate_text("x"));
    }

    #[test]
    fn footer_same_bottom_tiebreaks_by_block_index() {
        let mut page = candidate_page(400.0, 600.0);
        page.blocks = vec![
            candidate_block("first", 560.0, 590.0),
            candidate_block("second", 550.0, 590.0),
        ];

        let candidates = page_candidates(&page, Direction::Footer);
        let block_indices: Vec<_> = candidates.iter().map(|c| c.block_idx).collect();
        assert_eq!(block_indices, vec![0, 1]);
    }

    #[test]
    fn non_finite_or_non_positive_page_dimensions_have_no_candidates() {
        let invalid = [
            0.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];

        for value in invalid {
            let width_page = candidate_page(value, 600.0);
            assert!(page_candidates(&width_page, Direction::Header).is_empty());
            assert!(page_candidates(&width_page, Direction::Footer).is_empty());

            let height_page = candidate_page(400.0, value);
            assert!(page_candidates(&height_page, Direction::Header).is_empty());
            assert!(page_candidates(&height_page, Direction::Footer).is_empty());
        }
    }

    #[test]
    fn mirror_pair_basic() {
        assert_eq!(mirror_pair_values("Page 1", "Page 2"), Some((1, 2)));
        assert_eq!(mirror_pair_values("1", "2"), Some((1, 2)));
        // 数字連が2個は鏡映不一致
        assert_eq!(mirror_pair_values("1 of 2", "2 of 3"), None);
    }
}
