//! ストリームフィルタの復号
//!
//! 準拠: pdf.js `parser.js` makeFilter、および各 *\_stream.js

use std::io::Read;

use flate2::read::{DeflateDecoder, ZlibDecoder};

use crate::error::{Error, Result};

use super::crypto::CipherFactory;
use super::object::{Dict, Object, Stream};
use super::parser::Resolver;

/// PDF ストリームフィルタの種類
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum FilterKind {
    Flate,
    Lzw,
    AsciiHex,
    Ascii85,
    RunLength,
    Crypt,
}

impl FilterKind {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "Fl" | "FlateDecode" => FilterKind::Flate,
            "LZW" | "LZWDecode" => FilterKind::Lzw,
            "AHx" | "ASCIIHexDecode" => FilterKind::AsciiHex,
            "A85" | "ASCII85Decode" => FilterKind::Ascii85,
            "RL" | "RunLengthDecode" => FilterKind::RunLength,
            "Crypt" => FilterKind::Crypt,
            _ => return None,
        })
    }
}

/// Stream の /Filter 連鎖を適用して復号済みバイト列を返す
///
/// 暗号化がある場合はフィルタ適用前にストリーム本文を復号する。
/// /Crypt が明示されているときは二重適用しない。xref ストリームは cipher=None で呼ぶ。
/// limit はフィルタチェーン累積出力の上限バイト数
#[allow(dead_code)]
pub(crate) fn decode_stream(
    data: &[u8],
    stream: &Stream,
    resolver: &dyn Resolver,
    cipher: Option<&CipherFactory>,
    limit: usize,
) -> Result<Vec<u8>> {
    let end = stream
        .start
        .checked_add(stream.length)
        .ok_or_else(|| Error::Reader("stream range overflow".into()))?;
    if end > data.len() {
        return Err(Error::Reader(format!(
            "stream range out of bounds: start={}, length={}, data_len={}",
            stream.start,
            stream.length,
            data.len()
        )));
    }
    let mut decoded = data[stream.start..end].to_vec();

    let filter_obj = stream
        .dict
        .get2("F", "Filter")
        .map(|obj| resolve_owned(obj, resolver))
        .transpose()?;
    let params_root = stream
        .dict
        .get2("DP", "DecodeParms")
        .map(|o| resolve_owned(o, resolver))
        .transpose()?;

    // 暗号化: /Crypt が無いとき既定 StmF で先に復号。xref は crypto_ref 無し
    let has_crypt = filter_list_has_crypt(filter_obj.as_ref());
    let skip_crypto = is_xref_stream(&stream.dict)
        || is_unencrypted_metadata(&stream.dict, cipher);
    if !has_crypt && !skip_crypto
        && let Some(factory) = cipher
        && let Some(r) = stream.crypto_ref
    {
        decoded = factory.decrypt_stream(&decoded, r.num, r.generation, None)?;
    }

    let Some(filter_obj) = filter_obj else {
        return Ok(decoded);
    };

    let stages: Vec<(FilterKind, Option<Dict>)> = match filter_obj {
        Object::Name(name) => {
            let kind = FilterKind::from_name(&name)
                .ok_or_else(|| Error::Reader(format!("unsupported filter: \"{name}\"")))?;
            let params = match params_root {
                Some(Object::Dict(d)) => Some(d),
                // Name のとき Array の DecodeParms は無視（pdf.js は警告のみ）
                _ => None,
            };
            vec![(kind, params)]
        }
        Object::Array(filters) => {
            let mut stages = Vec::with_capacity(filters.len());
            for (i, f) in filters.iter().enumerate() {
                let f = resolve_owned(f, resolver)?;
                let Object::Name(name) = f else {
                    return Err(Error::Reader(format!("Bad filter name: {f:?}")));
                };
                let kind = FilterKind::from_name(&name)
                    .ok_or_else(|| Error::Reader(format!("unsupported filter: \"{name}\"")))?;
                let params = match &params_root {
                    Some(Object::Array(arr)) => {
                        arr.get(i).map(|p| resolve_owned(p, resolver)).transpose()?
                    }
                    // 単一 Dict を全段に使うのは仕様外。pdf.js は配列時のみインデックス参照
                    _ => None,
                };
                let params_dict = match params {
                    Some(Object::Dict(d)) => Some(d),
                    _ => None,
                };
                stages.push((kind, params_dict));
            }
            stages
        }
        Object::Null => Vec::new(),
        other => {
            return Err(Error::Reader(format!("invalid /Filter value: {other:?}")));
        }
    };

    apply_stages(decoded, &stages, cipher, stream.crypto_ref, limit)
}

/// フィルタ連鎖の適用
///
/// 伸長爆弾対策としてチェーン累積出力を budget で制限する
fn apply_stages(
    mut decoded: Vec<u8>,
    stages: &[(FilterKind, Option<Dict>)],
    cipher: Option<&CipherFactory>,
    crypto_ref: Option<super::object::Ref>,
    budget: usize,
) -> Result<Vec<u8>> {
    let mut remaining = budget;
    for (kind, params) in stages {
        decoded = apply_filter(&decoded, *kind, params.as_ref(), cipher, crypto_ref, remaining)?;
        if decoded.len() > remaining {
            return Err(Error::Reader("decoded stream output exceeds limit".into()));
        }
        remaining -= decoded.len();
    }
    Ok(decoded)
}

fn filter_list_has_crypt(filter_obj: Option<&Object>) -> bool {
    let is_crypt =
        |o: &Object| matches!(o, Object::Name(n) if FilterKind::from_name(n) == Some(FilterKind::Crypt));
    match filter_obj {
        Some(Object::Name(n)) => FilterKind::from_name(n) == Some(FilterKind::Crypt),
        Some(Object::Array(arr)) => arr.iter().any(is_crypt),
        _ => false,
    }
}

fn is_xref_stream(dict: &Dict) -> bool {
    matches!(dict.get("Type"), Some(Object::Name(n)) if n == "XRef")
}

fn is_unencrypted_metadata(dict: &Dict, cipher: Option<&CipherFactory>) -> bool {
    let Some(factory) = cipher else {
        return false;
    };
    if factory.encrypt_metadata() {
        return false;
    }
    matches!(dict.get("Type"), Some(Object::Name(n)) if n == "Metadata")
}

fn resolve_owned(obj: &Object, resolver: &dyn Resolver) -> Result<Object> {
    Ok(match obj {
        Object::Ref(r) => resolver.resolve(*r)?.unwrap_or(Object::Null),
        other => other.clone(),
    })
}

fn apply_filter(
    data: &[u8],
    kind: FilterKind,
    params: Option<&Dict>,
    cipher: Option<&CipherFactory>,
    crypto_ref: Option<super::object::Ref>,
    limit: usize,
) -> Result<Vec<u8>> {
    match kind {
        FilterKind::Flate => {
            let decoded = decode_flate(data, limit)?;
            apply_predictor_if_needed(decoded, params)
        }
        FilterKind::Lzw => {
            let early_change = params
                .and_then(|d| d.get("EarlyChange"))
                .and_then(as_i64)
                .unwrap_or(1);
            let decoded = decode_lzw(data, early_change, limit)?;
            apply_predictor_if_needed(decoded, params)
        }
        FilterKind::AsciiHex => Ok(decode_ascii_hex(data)),
        FilterKind::Ascii85 => Ok(decode_ascii85(data)),
        FilterKind::RunLength => decode_run_length(data, limit),
        FilterKind::Crypt => {
            // 明示 /Crypt: 既定 StmF を先に掛けていない経路でここで復号
            let Some(factory) = cipher else {
                return Ok(data.to_vec());
            };
            let Some(r) = crypto_ref else {
                return Ok(data.to_vec());
            };
            let filter_name = params
                .and_then(|d| d.get("Name"))
                .and_then(|o| match o {
                    Object::Name(n) => Some(n.as_ref()),
                    _ => None,
                });
            factory.decrypt_stream(data, r.num, r.generation, filter_name)
        }
    }
}

fn as_i64(obj: &Object) -> Option<i64> {
    match obj {
        Object::Int(n) => Some(*n),
        Object::Real(n) => Some(*n as i64),
        _ => None,
    }
}

// --- FlateDecode ---

/// zlib → raw deflate の順で試し、失敗時は途中まで読めた分を返す
///
/// 破損による途中終了は許容し、上限超過は Err にする
fn decode_flate(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    if let Some(out) = read_to_end_ok(ZlibDecoder::new(data), limit)? {
        return Ok(out);
    }
    if let Some(out) = read_to_end_ok(DeflateDecoder::new(data), limit)? {
        return Ok(out);
    }
    let partial = read_partial(ZlibDecoder::new(data), limit)?;
    if !partial.is_empty() {
        return Ok(partial);
    }
    read_partial(DeflateDecoder::new(data), limit)
}

/// 終端まで読めたら Some、読み取りエラーは None、上限超過は Err
fn read_to_end_ok<R: Read>(r: R, limit: usize) -> Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    let take_len = (limit as u64).saturating_add(1);
    match r.take(take_len).read_to_end(&mut out) {
        Ok(_) if out.len() > limit => {
            Err(Error::Reader("FlateDecode output exceeds limit".into()))
        }
        Ok(_) => Ok(Some(out)),
        Err(_) => Ok(None),
    }
}

/// 逐次読みで得られる分だけ保持する（壊れたストリーム耐性）
fn read_partial<R: Read>(mut r: R, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if out.len() > limit {
            return Err(Error::Reader("FlateDecode output exceeds limit".into()));
        }
        let want = (limit.saturating_add(1) - out.len()).min(buf.len());
        match r.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    Ok(out)
}

// --- LZWDecode ---

/// LZW 復号（辞書 4096・コード長 9→12・Clear=256・EOD=257）
///
/// 出力が limit を超えたら Err
fn decode_lzw(data: &[u8], early_change: i64, limit: usize) -> Result<Vec<u8>> {
    let early_change = u32::from(early_change != 0);
    const MAX_DICT: usize = 4096;
    const CLEAR: u32 = 256;
    const EOD: u32 = 257;

    let mut dictionary_values: Box<[u8; MAX_DICT]> = Box::new([0u8; MAX_DICT]);
    let mut dictionary_lengths: Box<[u16; MAX_DICT]> = Box::new([0u16; MAX_DICT]);
    let mut dictionary_prev_codes: Box<[u16; MAX_DICT]> = Box::new([0u16; MAX_DICT]);
    for i in 0..256 {
        dictionary_values[i] = i as u8;
        dictionary_lengths[i] = 1;
    }

    let mut code_length: u32 = 9;
    let mut next_code: u32 = 258;
    let mut current_sequence: Box<[u8; MAX_DICT]> = Box::new([0u8; MAX_DICT]);
    let mut current_sequence_length: usize = 0;
    let mut prev_code: u32 = 0;

    let mut bit_pos: usize = 0;
    let bit_len = data.len() * 8;

    let mut out = Vec::new();

    loop {
        let code = match read_bits_msb(data, &mut bit_pos, bit_len, code_length) {
            Some(c) => c,
            None => break,
        };

        let has_prev = current_sequence_length > 0;

        if code < 256 {
            current_sequence[0] = code as u8;
            current_sequence_length = 1;
        } else if code >= 258 {
            if (code as usize) < next_code as usize {
                let mut q = code as usize;
                current_sequence_length = dictionary_lengths[q] as usize;
                if current_sequence_length > MAX_DICT {
                    return Err(Error::Reader("LZW dictionary sequence too long".into()));
                }
                for j in (0..current_sequence_length).rev() {
                    current_sequence[j] = dictionary_values[q];
                    q = dictionary_prev_codes[q] as usize;
                }
            } else {
                // KwKwK: 直前系列 + その先頭
                if current_sequence_length >= MAX_DICT {
                    return Err(Error::Reader("LZW KwKwK overflow".into()));
                }
                current_sequence[current_sequence_length] = current_sequence[0];
                current_sequence_length += 1;
            }
        } else if code == CLEAR {
            code_length = 9;
            next_code = 258;
            current_sequence_length = 0;
            continue;
        } else if code == EOD {
            break;
        } else {
            break;
        }

        if has_prev && (next_code as usize) < MAX_DICT {
            dictionary_prev_codes[next_code as usize] = prev_code as u16;
            dictionary_lengths[next_code as usize] =
                dictionary_lengths[prev_code as usize].saturating_add(1);
            dictionary_values[next_code as usize] = current_sequence[0];
            next_code += 1;
            let n = next_code + early_change;
            // n が 2 の冪のときコード長を延ばす
            if n > 0 && (n & (n - 1)) == 0 {
                code_length = (n.ilog2() + 1).min(12);
            }
        }
        prev_code = code;
        if current_sequence_length > limit.saturating_sub(out.len()) {
            return Err(Error::Reader("LZWDecode output exceeds limit".into()));
        }
        out.extend_from_slice(&current_sequence[..current_sequence_length]);
    }

    Ok(out)
}

fn read_bits_msb(data: &[u8], bit_pos: &mut usize, bit_len: usize, n: u32) -> Option<u32> {
    if n == 0 {
        return Some(0);
    }
    if *bit_pos + n as usize > bit_len {
        return None;
    }
    let mut value = 0u32;
    for _ in 0..n {
        let byte = data[*bit_pos / 8];
        let bit = (byte >> (7 - (*bit_pos % 8))) & 1;
        value = (value << 1) | u32::from(bit);
        *bit_pos += 1;
    }
    Some(value)
}

// --- ASCIIHexDecode ---

fn decode_ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2);
    let mut first: Option<u8> = None;
    for &ch in data {
        let digit = if (b'0'..=b'9').contains(&ch) {
            ch & 0x0f
        } else if (b'A'..=b'F').contains(&ch) || (b'a'..=b'f').contains(&ch) {
            (ch & 0x0f) + 9
        } else if ch == b'>' {
            break;
        } else {
            // 空白・不正文字は無視
            continue;
        };
        match first {
            None => first = Some(digit),
            Some(hi) => {
                out.push((hi << 4) | digit);
                first = None;
            }
        }
    }
    if let Some(hi) = first {
        // 奇数桁は末尾 0 補完
        out.push(hi << 4);
    }
    out
}

// --- ASCII85Decode ---

fn decode_ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    let n = data.len();

    while i < n {
        while i < n && is_pdf_whitespace(data[i]) {
            i += 1;
        }
        if i >= n {
            break;
        }
        let c = data[i];
        if c == b'~' {
            // "~>" 終端
            break;
        }
        if c == b'z' {
            out.extend_from_slice(&[0, 0, 0, 0]);
            i += 1;
            continue;
        }

        let mut input = [0u8; 5];
        input[0] = c;
        i += 1;
        let mut count = 1usize;
        while count < 5 && i < n {
            while i < n && is_pdf_whitespace(data[i]) {
                i += 1;
            }
            if i >= n {
                break;
            }
            let c = data[i];
            if c == b'~' {
                break;
            }
            input[count] = c;
            count += 1;
            i += 1;
        }

        let out_bytes = if count < 5 {
            // 端数: 'u' でパディングし、count-1 バイトだけ採用
            for k in count..5 {
                input[k] = 0x21 + 84; // 'u'
            }
            count.saturating_sub(1)
        } else {
            4
        };

        let mut t: u32 = 0;
        for k in 0..5 {
            t = t
                .wrapping_mul(85)
                .wrapping_add(u32::from(input[k].wrapping_sub(0x21)));
        }
        let bytes = t.to_be_bytes();
        out.extend_from_slice(&bytes[..out_bytes]);

        if count < 5 {
            break;
        }
    }
    out
}

pub(super) fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0a | 0x0c | 0x0d | 0x20)
}

// --- RunLengthDecode ---

/// RunLength 復号
///
/// 出力が limit を超えたら Err
fn decode_run_length(data: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let n = data[i];
        if n == 128 {
            // EOD
            break;
        }
        i += 1;
        if n < 128 {
            // 次の n+1 バイトをコピー
            let len = (n as usize + 1).min(data.len() - i);
            if len > limit.saturating_sub(out.len()) {
                return Err(Error::Reader("RunLengthDecode output exceeds limit".into()));
            }
            out.extend_from_slice(&data[i..i + len]);
            i += len;
        } else {
            // 次の 1 バイトを (257-n) 回反復
            if i >= data.len() {
                break;
            }
            let b = data[i];
            i += 1;
            let times = 257 - n as usize;
            if times > limit.saturating_sub(out.len()) {
                return Err(Error::Reader("RunLengthDecode output exceeds limit".into()));
            }
            out.extend(std::iter::repeat(b).take(times));
        }
    }
    Ok(out)
}

// --- Predictor ---

fn apply_predictor_if_needed(data: Vec<u8>, params: Option<&Dict>) -> Result<Vec<u8>> {
    let Some(params) = params else {
        return Ok(data);
    };
    let predictor = params.get("Predictor").and_then(as_i64).unwrap_or(1);
    if predictor <= 1 {
        return Ok(data);
    }
    apply_predictor(&data, params, predictor)
}

fn apply_predictor(data: &[u8], params: &Dict, predictor: i64) -> Result<Vec<u8>> {
    if predictor != 2 && !(10..=15).contains(&predictor) {
        return Err(Error::Reader(format!(
            "unsupported predictor: {predictor}"
        )));
    }

    let colors = params.get("Colors").and_then(as_i64).unwrap_or(1).max(1) as usize;
    let bits = params
        .get2("BPC", "BitsPerComponent")
        .and_then(as_i64)
        .unwrap_or(8)
        .max(1) as usize;
    let columns = params.get("Columns").and_then(as_i64).unwrap_or(1).max(1) as usize;

    // 宣言値が過大なとき積のオーバーフローを弾く
    let overflow = || Error::Reader("predictor parameters overflow".into());
    let pix_bytes = colors
        .checked_mul(bits)
        .and_then(|v| v.checked_add(7))
        .map(|v| v >> 3)
        .ok_or_else(overflow)?;
    let row_bytes = columns
        .checked_mul(colors)
        .and_then(|v| v.checked_mul(bits))
        .and_then(|v| v.checked_add(7))
        .map(|v| v >> 3)
        .ok_or_else(overflow)?;

    if predictor == 2 {
        if bits != 8 {
            return Err(Error::Reader(format!(
                "TIFF predictor only supports 8-bit, got BitsPerComponent={bits}"
            )));
        }
        return Ok(predict_tiff8(data, colors, row_bytes));
    }

    // PNG predictor (10-15): 行頭 1 バイトがフィルタ種
    Ok(predict_png(data, pix_bytes, row_bytes)?)
}

fn predict_tiff8(data: &[u8], colors: usize, row_bytes: usize) -> Vec<u8> {
    if row_bytes == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(data.len());
    for raw in data.chunks_exact(row_bytes) {
        let start = out.len();
        out.extend_from_slice(raw);
        let row = &mut out[start..];
        for i in colors..row_bytes {
            row[i] = row[i - colors].wrapping_add(row[i]);
        }
    }
    out
}

fn predict_png(data: &[u8], pix_bytes: usize, row_bytes: usize) -> Result<Vec<u8>> {
    if row_bytes == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut prev = vec![0u8; row_bytes];
    let mut row = vec![0u8; row_bytes];

    // 行は「フィルタ種 1 バイト + 本体」。不完全な最終行は打ち切り
    for chunk in data.chunks_exact(1 + row_bytes) {
        let filter_type = chunk[0];
        let raw = &chunk[1..];

        match filter_type {
            0 => {
                // None
                row.copy_from_slice(raw);
            }
            1 => {
                // Sub
                for i in 0..pix_bytes.min(row_bytes) {
                    row[i] = raw[i];
                }
                for i in pix_bytes..row_bytes {
                    row[i] = raw[i].wrapping_add(row[i - pix_bytes]);
                }
            }
            2 => {
                // Up
                for i in 0..row_bytes {
                    row[i] = raw[i].wrapping_add(prev[i]);
                }
            }
            3 => {
                // Average
                for i in 0..pix_bytes.min(row_bytes) {
                    row[i] = raw[i].wrapping_add(prev[i] / 2);
                }
                for i in pix_bytes..row_bytes {
                    let avg = ((u16::from(prev[i]) + u16::from(row[i - pix_bytes])) / 2) as u8;
                    row[i] = raw[i].wrapping_add(avg);
                }
            }
            4 => {
                // Paeth
                for i in 0..pix_bytes.min(row_bytes) {
                    row[i] = raw[i].wrapping_add(paeth_predictor(0, prev[i], 0));
                }
                for i in pix_bytes..row_bytes {
                    let left = row[i - pix_bytes];
                    let up = prev[i];
                    let up_left = prev[i - pix_bytes];
                    row[i] = raw[i].wrapping_add(paeth_predictor(left, up, up_left));
                }
            }
            other => {
                return Err(Error::Reader(format!(
                    "unsupported PNG predictor filter type: {other}"
                )));
            }
        }
        out.extend_from_slice(&row);
        std::mem::swap(&mut prev, &mut row);
    }
    Ok(out)
}

fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let a = i16::from(a);
    let b = i16::from(b);
    let c = i16::from(c);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::object::Ref;
    use super::super::parser::{NullResolver, Resolver};
    use crate::extract::DEFAULT_MAX_DECODED_BYTES as MAX_DECODED_BYTES;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    fn make_stream(dict: Dict, data: &[u8]) -> (Vec<u8>, Stream) {
        let buf = data.to_vec();
        let stream = Stream::new(dict, 0, buf.len());
        (buf, stream)
    }

    fn dict_filter(name: &str) -> Dict {
        let mut d = Dict::new();
        d.set("Filter", Object::Name(name.to_string().into()));
        d
    }

    // ----- Flate -----

    #[test]
    fn flate_roundtrip() {
        let plain = b"Hello, FlateDecode! ".repeat(20);
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&plain).unwrap();
        let compressed = enc.finish().unwrap();

        let dict = dict_filter("FlateDecode");
        let (buf, stream) = make_stream(dict, &compressed);
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn flate_abbreviation_fl() {
        let plain = b"short";
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(plain).unwrap();
        let compressed = enc.finish().unwrap();
        let mut dict = Dict::new();
        dict.set("F", Object::Name("Fl".into()));
        let (buf, stream) = make_stream(dict, &compressed);
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn flate_partial_on_truncated() {
        let plain = b"abcdefghijklmnopqrstuvwxyz0123456789".repeat(10);
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&plain).unwrap();
        let mut compressed = enc.finish().unwrap();
        // 途中で切って壊す
        assert!(compressed.len() > 8);
        compressed.truncate(compressed.len() / 2);
        // ゴミを足す
        compressed.extend_from_slice(&[0xFF, 0x00, 0xAB]);

        let partial = decode_flate(&compressed, MAX_DECODED_BYTES).unwrap();
        // 完全復元はできないが、何かは読めているか、少なくとも panic しない
        // 壊し方によっては空もあり得るが、通常は先頭付近が読める
        let _ = &partial;
        // 明示: 完全一致はしない
        assert_ne!(partial, plain.to_vec());
    }

    #[test]
    fn flate_partial_returns_prefix_when_possible() {
        // 正常圧縮の後に余分バイト → 多くは全文復号できる（耐性確認）
        let plain = b"partial-prefix-data-ok";
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(plain).unwrap();
        let mut compressed = enc.finish().unwrap();
        compressed.extend_from_slice(b"GARBAGE");
        let out = decode_flate(&compressed, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, plain);
    }

    // ----- LZW -----

    /// テスト用 LZW 符号化器（pdf.js と同じビット詰め・EarlyChange）
    fn encode_lzw(input: &[u8], early_change: i64) -> Vec<u8> {
        let early_change = if early_change != 0 { 1 } else { 0 };
        // 単純実装: 各バイトをクリア後にリテラルとして出す（辞書成長の検証用）
        // 実用エンコーダ: 最長一致
        const MAX_DICT: usize = 4096;
        let mut dict: std::collections::HashMap<Vec<u8>, u32> = std::collections::HashMap::new();
        for i in 0..256u32 {
            dict.insert(vec![i as u8], i);
        }
        let mut next_code: u32 = 258;
        let mut code_length: u32 = 9;

        let mut bits: Vec<bool> = Vec::new();
        let write_code = |bits: &mut Vec<bool>, code: u32, len: u32| {
            for i in (0..len).rev() {
                bits.push(((code >> i) & 1) != 0);
            }
        };

        write_code(&mut bits, 256, code_length); // Clear

        let mut w: Vec<u8> = Vec::new();
        for &b in input {
            let mut wk = w.clone();
            wk.push(b);
            if dict.contains_key(&wk) {
                w = wk;
            } else {
                let code = dict[&w];
                write_code(&mut bits, code, code_length);
                if (next_code as usize) < MAX_DICT {
                    dict.insert(wk, next_code);
                    next_code += 1;
                    let n = next_code + early_change as u32;
                    if n > 0 && (n & (n - 1)) == 0 {
                        let new_len = ((n as f64).log2().floor() as u32).saturating_add(1);
                        code_length = new_len.min(12);
                    }
                }
                w = vec![b];
            }
        }
        if !w.is_empty() {
            write_code(&mut bits, dict[&w], code_length);
        }
        write_code(&mut bits, 257, code_length); // EOD

        // bits → bytes (MSB first)
        let mut out = Vec::new();
        for chunk in bits.chunks(8) {
            let mut byte = 0u8;
            for (i, &bit) in chunk.iter().enumerate() {
                if bit {
                    byte |= 1 << (7 - i);
                }
            }
            out.push(byte);
        }
        out
    }

    #[test]
    fn lzw_known_vector() {
        let plain = b"ABABABABABABABAB";
        let compressed = encode_lzw(plain, 1);
        let mut dict = dict_filter("LZWDecode");
        let mut parms = Dict::new();
        parms.set("EarlyChange", Object::Int(1));
        dict.set("DecodeParms", Object::Dict(parms));
        let (buf, stream) = make_stream(dict, &compressed);
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn lzw_early_change_0() {
        let plain = b"Hello LZW EarlyChange0 test data 0123456789";
        let compressed = encode_lzw(plain, 0);
        let mut dict = dict_filter("LZW");
        let mut parms = Dict::new();
        parms.set("EarlyChange", Object::Int(0));
        dict.set("DecodeParms", Object::Dict(parms));
        let (buf, stream) = make_stream(dict, &compressed);
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, plain);
    }

    // ----- ASCIIHex -----

    #[test]
    fn ascii_hex_basic() {
        let data = b"48656C6C6F>";
        assert_eq!(decode_ascii_hex(data), b"Hello");
    }

    #[test]
    fn ascii_hex_odd_digit() {
        // "ABC" → AB C0
        assert_eq!(decode_ascii_hex(b"ABC>"), vec![0xAB, 0xC0]);
    }

    #[test]
    fn ascii_hex_whitespace() {
        assert_eq!(decode_ascii_hex(b"48 65\n6C\t6C 6F>"), b"Hello");
    }

    #[test]
    fn ascii_hex_via_stream() {
        let mut dict = Dict::new();
        dict.set("F", Object::Name("AHx".into()));
        let (buf, stream) = make_stream(dict, b"DEADBEEF>");
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    // ----- ASCII85 -----

    #[test]
    fn ascii85_basic() {
        // "Hello" in ASCII85 is "87cURD]j7BEbo80" with ~>
        // Classic example: <~9jqo^~> = "Man "
        assert_eq!(decode_ascii85(b"9jqo^~>"), b"Man ");
    }

    #[test]
    fn ascii85_z_shorthand() {
        assert_eq!(decode_ascii85(b"z~>"), [0, 0, 0, 0]);
    }

    #[test]
    fn ascii85_partial_group() {
        // "1" encoded: single char group. Manual: encode 0x31 with padding.
        // From Adobe: "!!" partial… より確実な往復を自前で:
        // full group for 0x00000000 is z; for "Man " is 9jqo^
        // 端数: "H" = 0x48. 5 文字分にパディングして 1 バイト出力
        // t が 0x48000000 になる 5 文字を作るのは面倒なので、既知の
        // "9jqo" (4 chars → 3 bytes "Man") を使う
        let out = decode_ascii85(b"9jqo~>");
        assert_eq!(out, b"Man");
    }

    #[test]
    fn ascii85_via_stream() {
        let mut dict = Dict::new();
        dict.set("Filter", Object::Name("ASCII85Decode".into()));
        let (buf, stream) = make_stream(dict, b"9jqo^~>");
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, b"Man ");
    }

    // ----- RunLength -----

    #[test]
    fn run_length_copy_and_repeat() {
        // copy 3 bytes: n=2, then ABC; repeat 'X' 4 times: n=257-4=253, X; EOD
        let data = [
            2, b'A', b'B', b'C', // copy 3
            253, b'X', // repeat X × 4
            128, // EOD
        ];
        assert_eq!(decode_run_length(&data, MAX_DECODED_BYTES).unwrap(), b"ABCXXXX");
    }

    #[test]
    fn run_length_via_stream() {
        let mut dict = Dict::new();
        dict.set("Filter", Object::Name("RunLengthDecode".into()));
        let data = [0, b'Z', 128]; // copy 1 byte 'Z'
        let (buf, stream) = make_stream(dict, &data);
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, b"Z");
    }

    // ----- 出力上限 -----

    #[test]
    fn flate_limit_exceeded() {
        let plain = vec![0u8; 10_000];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&plain).unwrap();
        let compressed = enc.finish().unwrap();
        // 正常ストリームでも上限を超えたら部分読みに落ちず Err
        assert!(decode_flate(&compressed, 100).is_err());
        assert_eq!(decode_flate(&compressed, 10_000).unwrap(), plain);
    }

    #[test]
    fn lzw_limit_exceeded() {
        let plain = b"A".repeat(1000);
        let compressed = encode_lzw(&plain, 1);
        assert!(decode_lzw(&compressed, 1, 100).is_err());
        assert_eq!(decode_lzw(&compressed, 1, 1000).unwrap(), plain);
    }

    #[test]
    fn run_length_limit_exceeded() {
        // repeat 'A' × 128
        let repeat = [129, b'A'];
        assert!(decode_run_length(&repeat, 100).is_err());
        assert_eq!(decode_run_length(&repeat, 128).unwrap().len(), 128);
        // copy 128 bytes
        let mut copy = vec![127u8];
        copy.extend_from_slice(&[b'B'; 128]);
        assert!(decode_run_length(&copy, 100).is_err());
        assert_eq!(decode_run_length(&copy, 128).unwrap().len(), 128);
    }

    #[test]
    fn chain_budget_is_cumulative() {
        // RunLength → 256 バイトの hex 文字列 → AsciiHex → 128 バイト
        let input = vec![129, b'3', 129, b'4'];
        let stages = vec![
            (FilterKind::RunLength, None),
            (FilterKind::AsciiHex, None),
        ];
        // 累積 384 バイト: budget 512 なら通る
        let out = apply_stages(input.clone(), &stages, None, None, 512).unwrap();
        assert_eq!(out.len(), 128);
        assert_eq!(out[0], 0x33);
        assert_eq!(out[127], 0x44);
        // budget 300 は 1 段目 256 で残り 44、2 段目 128 で超過
        assert!(apply_stages(input, &stages, None, None, 300).is_err());
    }

    #[test]
    fn chain_later_stage_gets_remaining_budget() {
        // 1 段目: 128 バイトのリテラルコピー（64 組の反復指示）
        // 2 段目: その指示を展開して 8192 バイト
        let mut input = vec![127u8];
        for _ in 0..64 {
            input.extend_from_slice(&[129, b'B']);
        }
        let stages = vec![
            (FilterKind::RunLength, None),
            (FilterKind::RunLength, None),
        ];
        assert!(apply_stages(input.clone(), &stages, None, None, 1000).is_err());
        let out = apply_stages(input, &stages, None, None, 8192 + 128).unwrap();
        assert_eq!(out.len(), 8192);
    }

    // ----- PNG Predictor -----

    fn flate_with_predictor(plain_rows: &[u8], predictor_rows: &[u8], cols: i64, filter_byte_mode: bool) -> Vec<u8> {
        let _ = plain_rows;
        let _ = filter_byte_mode;
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(predictor_rows).unwrap();
        let compressed = enc.finish().unwrap();

        let mut parms = Dict::new();
        parms.set("Predictor", Object::Int(15)); // PNG optimum 相当（行ごと）
        parms.set("Colors", Object::Int(1));
        parms.set("BitsPerComponent", Object::Int(8));
        parms.set("Columns", Object::Int(cols));

        let mut dict = dict_filter("FlateDecode");
        dict.set("DecodeParms", Object::Dict(parms));
        let (buf, stream) = make_stream(dict, &compressed);
        decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap()
    }

    #[test]
    fn png_predictor_sub() {
        // 1 row, 4 columns, filter type Sub (1)
        // original: [10, 20, 30, 40]
        // sub encoded: [10, 10, 10, 10]  (diff from left)
        let encoded = [1, 10, 10, 10, 10];
        let out = flate_with_predictor(&[10, 20, 30, 40], &encoded, 4, true);
        assert_eq!(out, vec![10, 20, 30, 40]);
    }

    #[test]
    fn png_predictor_up() {
        // row0 None: [1,2,3], row1 Up of same → encoded zeros + filter 2
        let encoded = [
            0, 1, 2, 3, // row0
            2, 0, 0, 0, // row1 Up
        ];
        let out = flate_with_predictor(&[1, 2, 3, 1, 2, 3], &encoded, 3, true);
        assert_eq!(out, vec![1, 2, 3, 1, 2, 3]);
    }

    #[test]
    fn png_predictor_average() {
        // 1 row: original [8, 12], Average with prev=0, left
        // i=0: raw = 8 - 0/2 = 8
        // i=1: raw = 12 - (0+8)/2 = 12-4 = 8
        let encoded = [3, 8, 8];
        let out = flate_with_predictor(&[8, 12], &encoded, 2, true);
        assert_eq!(out, vec![8, 12]);
    }

    #[test]
    fn png_predictor_paeth() {
        // 1 row [5, 9], Paeth: left/up/upleft
        // i=0: paeth(0,0,0)=0 → raw=5
        // i=1: paeth(5,0,0)=5 → raw=9-5=4
        let encoded = [4, 5, 4];
        let out = flate_with_predictor(&[5, 9], &encoded, 2, true);
        assert_eq!(out, vec![5, 9]);
    }

    // ----- TIFF Predictor -----

    #[test]
    fn tiff_predictor_8bit() {
        // original row: [10, 20, 30, 40], colors=1
        // TIFF horizontal: [10, 10, 10, 10]
        let predicted = [10u8, 10, 10, 10];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&predicted).unwrap();
        let compressed = enc.finish().unwrap();

        let mut parms = Dict::new();
        parms.set("Predictor", Object::Int(2));
        parms.set("Colors", Object::Int(1));
        parms.set("BitsPerComponent", Object::Int(8));
        parms.set("Columns", Object::Int(4));

        let mut dict = dict_filter("FlateDecode");
        dict.set("DecodeParms", Object::Dict(parms));
        let (buf, stream) = make_stream(dict, &compressed);
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, vec![10, 20, 30, 40]);
    }

    // ----- Filter chain & DecodeParms array -----

    #[test]
    fn filter_chain_array() {
        // 内側: Flate、外側: ASCIIHex（PDF は外側が先に並ぶ = 最初に適用）
        let plain = b"chain-test-payload";
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(plain).unwrap();
        let compressed = enc.finish().unwrap();
        let mut hex = String::new();
        for b in &compressed {
            hex.push_str(&format!("{b:02X}"));
        }
        hex.push('>');
        let hex_bytes = hex.into_bytes();

        let mut dict = Dict::new();
        dict.set(
            "Filter",
            Object::Array(vec![
                Object::Name("ASCIIHexDecode".into()),
                Object::Name("FlateDecode".into()),
            ]),
        );
        let (buf, stream) = make_stream(dict, &hex_bytes);
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn decode_parms_array_index() {
        // [ASCIIHex, Flate+TIFF predictor]
        let predicted = [10u8, 10, 10, 10];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&predicted).unwrap();
        let compressed = enc.finish().unwrap();
        let mut hex = String::new();
        for b in &compressed {
            hex.push_str(&format!("{b:02X}"));
        }
        hex.push('>');

        let mut flate_parms = Dict::new();
        flate_parms.set("Predictor", Object::Int(2));
        flate_parms.set("Colors", Object::Int(1));
        flate_parms.set("BitsPerComponent", Object::Int(8));
        flate_parms.set("Columns", Object::Int(4));

        let mut dict = Dict::new();
        dict.set(
            "Filter",
            Object::Array(vec![
                Object::Name("AHx".into()),
                Object::Name("Fl".into()),
            ]),
        );
        dict.set(
            "DecodeParms",
            Object::Array(vec![Object::Null, Object::Dict(flate_parms)]),
        );
        let (buf, stream) = make_stream(dict, hex.as_bytes());
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, vec![10, 20, 30, 40]);
    }

    #[test]
    fn decode_parms_ref_resolved() {
        struct MapResolver;
        impl Resolver for MapResolver {
            fn resolve(&self, r: Ref) -> Result<Option<Object>> {
                if r.num == 5 {
                    let mut parms = Dict::new();
                    parms.set("Predictor", Object::Int(2));
                    parms.set("Columns", Object::Int(2));
                    parms.set("Colors", Object::Int(1));
                    parms.set("BitsPerComponent", Object::Int(8));
                    Ok(Some(Object::Dict(parms)))
                } else {
                    Ok(None)
                }
            }
        }

        // predicted [1, 2] → original [1, 3]
        let predicted = [1u8, 2];
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&predicted).unwrap();
        let compressed = enc.finish().unwrap();

        let mut dict = dict_filter("FlateDecode");
        dict.set("DecodeParms", Object::Ref(Ref::new(5, 0)));
        let (buf, stream) = make_stream(dict, &compressed);
        let out = decode_stream(&buf, &stream, &MapResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, vec![1, 3]);
    }

    #[test]
    fn unknown_filter_errors() {
        let dict = dict_filter("DCTDecode");
        let (buf, stream) = make_stream(dict, b"fake");
        let err = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unsupported filter"), "{msg}");
    }

    #[test]
    fn no_filter_returns_raw() {
        let data = b"raw-bytes";
        let (buf, stream) = make_stream(Dict::new(), data);
        let out = decode_stream(&buf, &stream, &NullResolver, None, MAX_DECODED_BYTES).unwrap();
        assert_eq!(out, data);
    }
}
