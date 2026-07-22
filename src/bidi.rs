//! PDF 文字列の bidi 並べ替え
//!
//! 移植元: refs/pdf.js/src/core/bidi.js（Unicode Bidirectional Algorithm のサブセット）

use BidiType::*;

/// bidi 結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidiText {
    pub str: String,
    pub dir: String,
}

/// `bidi` オプション有効時の並べ替え（RTL 基準固定）
pub fn apply_bidi(s: &str, vertical: bool) -> BidiText {
    bidi(s, 1, vertical)
}

/// Bidi クラス（UBA サブセット）。名称は UBA 仕様どおり
#[allow(non_camel_case_types)]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum BidiType {
    L, R, AL, AN, EN, ES, ET, CS, WS, ON, BN, NSM, S, B,
}

/// テキスト方向
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Dir {
    Ltr,
    Rtl,
    Ttb,
}

impl Dir {
    fn as_str(self) -> &'static str {
        match self {
            Dir::Ltr => "ltr",
            Dir::Rtl => "rtl",
            Dir::Ttb => "ttb",
        }
    }
}

fn create_bidi_text(str: String, is_ltr: bool, vertical: bool) -> BidiText {
    let dir = if vertical {
        Dir::Ttb
    } else if is_ltr {
        Dir::Ltr
    } else {
        Dir::Rtl
    };
    BidiText {
        str,
        dir: dir.as_str().to_string(),
    }
}

// U+0000..=U+00FF の Bidi クラス
#[rustfmt::skip]
const BASE_TYPES: [BidiType; 256] = [
    BN, BN, BN, BN, BN, BN, BN, BN, BN, S, B, S,
    WS, B, BN, BN, BN, BN, BN, BN, BN, BN, BN, BN,
    BN, BN, BN, BN, B, B, B, S, WS, ON, ON, ET,
    ET, ET, ON, ON, ON, ON, ON, ES, CS, ES, CS, CS,
    EN, EN, EN, EN, EN, EN, EN, EN, EN, EN, CS, ON,
    ON, ON, ON, ON, ON, L, L, L, L, L, L, L, L,
    L, L, L, L, L, L, L, L, L, L, L, L, L, L,
    L, L, L, L, ON, ON, ON, ON, ON, ON, L, L, L,
    L, L, L, L, L, L, L, L, L, L, L, L, L, L,
    L, L, L, L, L, L, L, L, L, ON, ON, ON, ON,
    BN, BN, BN, BN, BN, BN, B, BN, BN, BN, BN, BN,
    BN, BN, BN, BN, BN, BN, BN, BN, BN, BN, BN, BN,
    BN, BN, BN, BN, BN, BN, BN, BN, BN, CS, ON, ET,
    ET, ET, ET, ON, ON, ON, ON, L, ON, ON, BN, ON,
    ON, ET, ET, EN, EN, ON, L, ON, ON, ON, EN, L,
    ON, ON, ON, ON, ON, L, L, L, L, L, L, L, L,
    L, L, L, L, L, L, L, L, L, L, L, L, L, L,
    L, ON, L, L, L, L, L, L, L, L, L, L, L, L,
    L, L, L, L, L, L, L, L, L, L, L, L, L, L,
    L, L, L, L, L, ON, L, L, L, L, L, L, L, L,
];

// U+0600..=U+06FF。061D は欠番のため ON 扱い
#[rustfmt::skip]
const ARABIC_TYPES: [BidiType; 256] = [
    AN, AN, AN, AN, AN, AN, ON, ON, AL, ET, ET, AL,
    CS, AL, ON, ON, NSM, NSM, NSM, NSM, NSM, NSM, NSM, NSM,
    NSM, NSM, NSM, AL, AL, ON, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, NSM, NSM, NSM, NSM, NSM, NSM, NSM, NSM, NSM,
    NSM, NSM, NSM, NSM, NSM, NSM, NSM, NSM, NSM, NSM, NSM, NSM,
    AN, AN, AN, AN, AN, AN, AN, AN, AN, AN, ET, AN,
    AN, AL, AL, AL, NSM, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, AL,
    AL, AL, AL, AL, AL, AL, AL, AL, AL, AL, NSM, NSM,
    NSM, NSM, NSM, NSM, NSM, AN, ON, NSM, NSM, NSM, NSM, NSM,
    NSM, AL, AL, NSM, NSM, ON, NSM, NSM, NSM, NSM, AL, AL,
    EN, EN, EN, EN, EN, EN, EN, EN, EN, EN, AL, AL,
    AL, AL, AL, AL,
];

fn is_odd(i: i32) -> bool {
    (i & 1) != 0
}

fn is_even(i: i32) -> bool {
    (i & 1) == 0
}

fn find_unequal(arr: &[BidiType], start: usize, value: BidiType) -> usize {
    arr[start..]
        .iter()
        .position(|&t| t != value)
        .map(|i| start + i)
        .unwrap_or(arr.len())
}

fn char_type(ch: char) -> BidiType {
    let code = ch as u32;
    if code <= 0x00ff {
        BASE_TYPES[code as usize]
    } else if (0x0590..=0x05f4).contains(&code) {
        R
    } else if (0x0600..=0x06ff).contains(&code) {
        ARABIC_TYPES[(code & 0xff) as usize]
    } else if (0x0700..=0x08ac).contains(&code)
        || (0xfb50..=0xfdff).contains(&code)
        || (0xfe70..=0xfeff).contains(&code)
    {
        AL
    } else {
        L
    }
}

/// 最初に見つかった強い方向
fn strong_dir(iter: impl IntoIterator<Item = BidiType>, fallback: BidiType) -> BidiType {
    for t in iter {
        match t {
            L => return L,
            R | EN | AN => return R,
            _ => {}
        }
    }
    fallback
}

/// `start_level == -1` は文字比率で基準方向を推定（単体比較用）
/// 本番の `apply_bidi` は常に `start_level = 1`
pub fn bidi(s: &str, start_level: i32, vertical: bool) -> BidiText {
    if s.is_empty() || vertical {
        return create_bidi_text(s.to_string(), true, vertical);
    }

    // RTL 対象は BMP のみで char と UTF-16 コード単位が一致
    let mut chars: Vec<char> = s.chars().collect();
    let str_length = chars.len();
    let mut types: Vec<BidiType> = chars.iter().map(|&c| char_type(c)).collect();
    let num_bidi = types
        .iter()
        .filter(|&&t| matches!(t, R | AL | AN))
        .count();

    if num_bidi == 0 {
        return create_bidi_text(s.to_string(), true, false);
    }

    let mut is_ltr = true;
    let mut start_level = start_level;
    if start_level == -1 {
        if (num_bidi as f64) / (str_length as f64) < 0.3 && str_length > 4 {
            start_level = 0;
        } else {
            is_ltr = false;
            start_level = 1;
        }
    } else if is_odd(start_level) {
        // 明示 RTL 基準では dir も rtl
        is_ltr = false;
    }

    let mut levels = vec![start_level; str_length];
    let e = if is_odd(start_level) { R } else { L };
    let sor = e;
    let eor = sor;

    // W1. NSM → 直前の型
    let mut last_type = sor;
    for t in &mut types {
        if *t == NSM {
            *t = last_type;
        } else {
            last_type = *t;
        }
    }

    // W2. AL の後の EN → AN
    last_type = sor;
    for t in &mut types {
        match *t {
            EN if last_type == AL => *t = AN,
            R | L | AL => last_type = *t,
            _ => {}
        }
    }

    // W3. AL → R
    for t in &mut types {
        if *t == AL {
            *t = R;
        }
    }

    // W4. 数字に挟まれた ES/CS
    for i in 1..str_length.saturating_sub(1) {
        if types[i] == ES && types[i - 1] == EN && types[i + 1] == EN {
            types[i] = EN;
        }
        if types[i] == CS
            && matches!(types[i - 1], EN | AN)
            && types[i + 1] == types[i - 1]
        {
            types[i] = types[i - 1];
        }
    }

    // W5. EN 隣接の ET → EN
    for i in 0..str_length {
        if types[i] == EN {
            let mut j = i;
            while j > 0 && types[j - 1] == ET {
                j -= 1;
                types[j] = EN;
            }
            let mut j = i + 1;
            while j < str_length && types[j] == ET {
                types[j] = EN;
                j += 1;
            }
        }
    }

    // W6. WS/ES/ET/CS → ON
    for t in &mut types {
        if matches!(*t, WS | ES | ET | CS) {
            *t = ON;
        }
    }

    // W7. L の後の EN → L
    last_type = sor;
    for t in &mut types {
        match *t {
            EN if last_type == L => *t = L,
            R | L => last_type = *t,
            _ => {}
        }
    }

    // N1. 中立文字は左右の強い方向の一致時にその方向
    let mut i = 0;
    while i < str_length {
        if types[i] == ON {
            let end = find_unequal(&types, i + 1, ON);
            let before = strong_dir(types[..i].iter().rev().copied(), sor);
            let after = strong_dir(types[end..].iter().copied(), eor);
            if before == after {
                types[i..end].fill(before);
            }
            i = end;
            continue;
        }
        i += 1;
    }

    // N2. 残った中立文字は埋め込み方向
    for t in &mut types {
        if *t == ON {
            *t = e;
        }
    }

    // I1 / I2
    for (t, lv) in types.iter().zip(levels.iter_mut()) {
        if is_even(*lv) {
            match *t {
                R => *lv += 1,
                AN | EN => *lv += 2,
                _ => {}
            }
        } else if matches!(*t, L | AN | EN) {
            *lv += 1;
        }
    }

    // L2. 最上位レベルから最下位の奇数レベルまで、その水準以上の連続を逐次反転
    let highest_level = *levels.iter().max().unwrap_or(&-1);
    let lowest_odd_level = levels
        .iter()
        .copied()
        .filter(|&lv| is_odd(lv))
        .min()
        .unwrap_or(99);

    for level in (lowest_odd_level..=highest_level).rev() {
        let mut start: Option<usize> = None;
        for (i, &lv) in levels.iter().enumerate() {
            match (lv < level, start) {
                (true, Some(s)) => {
                    chars[s..i].reverse();
                    start = None;
                }
                (false, None) => start = Some(i),
                _ => {}
            }
        }
        if let Some(s) = start {
            chars[s..].reverse();
        }
    }

    // < と > は除去
    let out: String = chars
        .into_iter()
        .filter(|&c| c != '<' && c != '>')
        .collect();
    create_bidi_text(out, is_ltr, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_arabic_rtl_base_reverses() {
        let r = apply_bidi("مرحبا", false);
        assert_eq!(r.str, "ابحرم");
        assert_eq!(r.dir, "rtl");
    }

    #[test]
    fn pure_hebrew_rtl_base_reverses() {
        let r = apply_bidi("שלום", false);
        assert_eq!(r.str, "םולש");
        assert_eq!(r.dir, "rtl");
    }

    #[test]
    fn pure_latin_unchanged_ltr() {
        let r = apply_bidi("Hello", false);
        assert_eq!(r.str, "Hello");
        assert_eq!(r.dir, "ltr");
    }

    #[test]
    fn mixed_hello_arabic() {
        let r = apply_bidi("Hello مرحبا", false);
        assert_eq!(r.str, "ابحرم Hello");
        assert_eq!(r.dir, "rtl");
    }

    #[test]
    fn mixed_arabic_hello() {
        let r = apply_bidi("مرحبا Hello", false);
        assert_eq!(r.str, "Hello ابحرم");
        assert_eq!(r.dir, "rtl");
    }

    #[test]
    fn vertical_passthrough() {
        let r = apply_bidi("مرحبا", true);
        assert_eq!(r.str, "مرحبا");
        assert_eq!(r.dir, "ttb");
    }

    #[test]
    fn empty_string() {
        let r = apply_bidi("", false);
        assert_eq!(r.str, "");
        assert_eq!(r.dir, "ltr");
    }

    #[test]
    fn auto_mode_matches_pdfjs_pure_arabic() {
        let r = bidi("مرحبا", -1, false);
        assert_eq!(r.str, "ابحرم");
        assert_eq!(r.dir, "rtl");
    }

    #[test]
    fn digits_only_ltr() {
        let r = apply_bidi("123", false);
        assert_eq!(r.str, "123");
        assert_eq!(r.dir, "ltr");
    }

    #[test]
    fn visual_arabic_to_logical_via_rtl_base() {
        // 空間左→右（視覚順）を RTL 基準 bidi に通すと論理順に戻る
        let r = apply_bidi("ابحرم", false);
        assert_eq!(r.str, "مرحبا");
        assert_eq!(r.dir, "rtl");
    }
}
