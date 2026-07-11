//! PDFファイル構造の入口

use crate::error::{Error, Result};

/// PDFヘッダ署名
const PDF_HEADER_SIGNATURE: &[u8] = b"%PDF-";
/// ヘッダ探索範囲の上限
const HEADER_SCAN_LIMIT: usize = 1024;
/// startxref キーワード
const STARTXREF_SIGNATURE: &[u8] = b"startxref";
/// startxref 後方探索のステップ幅
const STARTXREF_STEP: usize = 1024;

/// 先頭付近の PDF 署名位置
pub(crate) fn find_pdf_header_offset(data: &[u8]) -> Result<usize> {
    let limit = data.len().min(HEADER_SCAN_LIMIT);
    if limit < PDF_HEADER_SIGNATURE.len() {
        return Err(Error::Reader("PDF header signature not found".into()));
    }
    let window = &data[..limit];
    window
        .windows(PDF_HEADER_SIGNATURE.len())
        .position(|w| w == PDF_HEADER_SIGNATURE)
        .ok_or_else(|| Error::Reader("PDF header signature not found".into()))
}

/// ファイル末尾側から得た startxref オフセット
pub(crate) fn find_startxref(data: &[u8]) -> Result<usize> {
    if data.len() < STARTXREF_SIGNATURE.len() {
        return Err(Error::Reader("startxref not found".into()));
    }

    let sig_len = STARTXREF_SIGNATURE.len();
    // 窓同士がキーワード長ぶん重なるよう末尾から広げる
    let mut pos = data.len();
    loop {
        pos = pos.saturating_sub(STARTXREF_STEP - sig_len);
        let end = (pos + STARTXREF_STEP).min(data.len());
        if let Some(rel) = rfind_bytes(&data[pos..end], STARTXREF_SIGNATURE) {
            let abs = pos + rel;
            return parse_offset_after_startxref(data, abs + sig_len);
        }
        if pos == 0 {
            break;
        }
    }
    Err(Error::Reader("startxref not found".into()))
}

/// スライス末尾側から部分列を探す
fn rfind_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    (0..=last).rev().find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// startxref 直後の整数オフセットを読む
fn parse_offset_after_startxref(data: &[u8], i: usize) -> Result<usize> {
    let invalid = || Error::Reader("startxref offset is missing or invalid".into());
    let rest = data.get(i..).unwrap_or_default();
    let ws = rest.iter().take_while(|&&b| is_pdf_whitespace(b)).count();
    let digits = rest[ws..].iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return Err(invalid());
    }
    std::str::from_utf8(&rest[ws..ws + digits])
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(invalid)
}

fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_at_start() {
        let data = b"%PDF-1.7\n...";
        assert_eq!(find_pdf_header_offset(data).unwrap(), 0);
    }

    #[test]
    fn header_after_garbage() {
        let mut data = vec![0x00, 0x01, 0x02, b'x', b'y'];
        data.extend_from_slice(b"%PDF-1.4\n");
        assert_eq!(find_pdf_header_offset(&data).unwrap(), 5);
    }

    #[test]
    fn header_not_found() {
        let data = b"not a pdf file at all";
        assert!(find_pdf_header_offset(data).is_err());
    }

    #[test]
    fn header_beyond_scan_limit() {
        let mut data = vec![b'x'; HEADER_SCAN_LIMIT];
        data.extend_from_slice(b"%PDF-1.7");
        assert!(find_pdf_header_offset(&data).is_err());
    }

    #[test]
    fn startxref_normal() {
        let data = b"%PDF-1.4\n...xref...\nstartxref\n12345\n%%EOF\n";
        assert_eq!(find_startxref(data).unwrap(), 12345);
    }

    #[test]
    fn startxref_with_crlf_and_spaces() {
        let data = b"%PDF-1.4\ntrailer\nstartxref\r\n  999\r\n%%EOF";
        assert_eq!(find_startxref(data).unwrap(), 999);
    }

    #[test]
    fn startxref_not_found() {
        let data = b"%PDF-1.4\nno xref pointer here\n%%EOF\n";
        assert!(find_startxref(data).is_err());
    }

    #[test]
    fn startxref_near_end_of_large_buffer() {
        let mut data = vec![b'.'; 3000];
        data.extend_from_slice(b"\nstartxref\n42\n%%EOF\n");
        assert_eq!(find_startxref(&data).unwrap(), 42);
    }
}
