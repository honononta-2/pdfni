//! .bcmap バイナリデコーダ
//!
//! 参考: pdf.js `binary_cmap.js`

use super::cmap::{CMap, MapValue};

const MAX_NUM_SIZE: usize = 16;
const MAX_ENCODED_NUM_SIZE: usize = 19;

/// デコード失敗
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BcmapError(pub String);

impl std::fmt::Display for BcmapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BcmapError {}

/// .bcmap ストリーム読み取り
pub(crate) struct BinaryCMapStream<'a> {
    data: &'a [u8],
    pos: usize,
    tmp: [u8; MAX_ENCODED_NUM_SIZE],
}

impl<'a> BinaryCMapStream<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            tmp: [0; MAX_ENCODED_NUM_SIZE],
        }
    }

    fn get_byte(&mut self) -> Result<u8, BcmapError> {
        if self.pos >= self.data.len() {
            return Err(BcmapError("unexpected EOF in bcmap".into()));
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn get_bytes(&mut self, n: usize) -> Result<&'a [u8], BcmapError> {
        if self.pos + n > self.data.len() {
            return Err(BcmapError("unexpected EOF in bcmap".into()));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// UN: base-128、bit7 継続、bits6-0 データ
    pub fn read_number(&mut self) -> Result<u32, BcmapError> {
        let mut n: u32 = 0;
        loop {
            let b = self.get_byte()?;
            n = (n << 7) | u32::from(b & 0x7f);
            if b & 0x80 == 0 {
                return Ok(n);
            }
        }
    }

    /// SN: zigzag
    pub fn read_signed(&mut self) -> Result<i32, BcmapError> {
        let n = self.read_number()?;
        if n & 1 != 0 {
            Ok(!((n >> 1) as i32))
        } else {
            Ok((n >> 1) as i32)
        }
    }

    /// B[size]: size+1 バイトを固定配列へ
    pub fn read_hex(&mut self, num: &mut [u8], size: usize) -> Result<(), BcmapError> {
        let bytes = self.get_bytes(size + 1)?;
        num[..=size].copy_from_slice(bytes);
        Ok(())
    }

    /// UB[size]: UN 値を size+1 バイト固定幅へ展開
    pub fn read_hex_number(&mut self, num: &mut [u8], size: usize) -> Result<(), BcmapError> {
        let mut sp = 0usize;
        loop {
            let b = self.get_byte()?;
            if sp >= MAX_ENCODED_NUM_SIZE {
                return Err(BcmapError("hex number too long".into()));
            }
            self.tmp[sp] = b & 0x7f;
            sp += 1;
            if b & 0x80 == 0 {
                break;
            }
        }
        let mut buffer: u32 = 0;
        let mut buffer_size: u32 = 0;
        for i in (0..=size).rev() {
            while buffer_size < 8 && sp > 0 {
                sp -= 1;
                buffer |= u32::from(self.tmp[sp]) << buffer_size;
                buffer_size += 7;
            }
            num[i] = (buffer & 255) as u8;
            buffer >>= 8;
            buffer_size = buffer_size.saturating_sub(8);
        }
        Ok(())
    }

    /// SB[size]: 符号付き固定幅（zigzag をバイト列で）
    pub fn read_hex_signed(&mut self, num: &mut [u8], size: usize) -> Result<(), BcmapError> {
        self.read_hex_number(num, size)?;
        let sign: u8 = if num[size] & 1 != 0 { 255 } else { 0 };
        let mut c: u16 = 0;
        for i in 0..=size {
            c = ((c & 1) << 8) | u16::from(num[i]);
            num[i] = ((c >> 1) as u8) ^ sign;
        }
        Ok(())
    }

    /// S: 長さ(UN) + 各文字(UN)
    pub fn read_string(&mut self) -> Result<String, BcmapError> {
        let len = self.read_number()? as usize;
        let mut out = String::with_capacity(len);
        for _ in 0..len {
            let ch = self.read_number()?;
            if let Some(c) = char::from_u32(ch) {
                out.push(c);
            } else {
                return Err(BcmapError(format!("invalid string codepoint {ch}")));
            }
        }
        Ok(out)
    }
}

fn hex_to_int(a: &[u8], size: usize) -> u32 {
    let mut n: u32 = 0;
    for i in 0..=size {
        n = (n << 8) | u32::from(a[i]);
    }
    n
}

fn hex_to_bytes(a: &[u8], size: usize) -> Vec<u8> {
    a[..=size].to_vec()
}

fn add_hex(a: &mut [u8], b: &[u8], size: usize) {
    let mut c: u16 = 0;
    for i in (0..=size).rev() {
        c += u16::from(a[i]) + u16::from(b[i]);
        a[i] = (c & 255) as u8;
        c >>= 8;
    }
}

fn inc_hex(a: &mut [u8], size: usize) {
    let mut c: u16 = 1;
    for i in (0..=size).rev() {
        if c == 0 {
            break;
        }
        c += u16::from(a[i]);
        a[i] = (c & 255) as u8;
        c >>= 8;
    }
}

/// .bcmap の CMap へのデコード。usecmap 名があれば返り値に
pub(crate) fn decode_bcmap(data: &[u8], cmap: &mut CMap) -> Result<Option<String>, BcmapError> {
    let mut stream = BinaryCMapStream::new(data);
    let header = stream.get_byte()?;
    cmap.set_vertical((header & 1) != 0);

    let mut use_cmap: Option<String> = None;
    let mut start = [0u8; MAX_NUM_SIZE];
    let mut end = [0u8; MAX_NUM_SIZE];
    let mut ch = [0u8; MAX_NUM_SIZE];
    let mut char_code = [0u8; MAX_NUM_SIZE];
    let mut tmp = [0u8; MAX_NUM_SIZE];

    while let Ok(b) = stream.get_byte() {
        let type_ = b >> 5;
        if type_ == 7 {
            match b & 0x1f {
                0 => {
                    let _ = stream.read_string()?;
                }
                1 => {
                    use_cmap = Some(stream.read_string()?);
                }
                _ => {}
            }
            continue;
        }

        let sequence = (b & 0x10) != 0;
        let data_size = (b & 0x0f) as usize;
        if data_size + 1 > MAX_NUM_SIZE {
            return Err(BcmapError("invalid dataSize".into()));
        }

        let ucs2_data_size = 1usize;
        let subitems_count = stream.read_number()? as usize;

        match type_ {
            0 => {
                // codespacerange
                stream.read_hex(&mut start, data_size)?;
                stream.read_hex_number(&mut end, data_size)?;
                add_hex(&mut end, &start, data_size);
                cmap.add_codespace_range(
                    data_size + 1,
                    hex_to_int(&start, data_size),
                    hex_to_int(&end, data_size),
                );
                for _ in 1..subitems_count {
                    inc_hex(&mut end, data_size);
                    stream.read_hex_number(&mut start, data_size)?;
                    add_hex(&mut start, &end, data_size);
                    stream.read_hex_number(&mut end, data_size)?;
                    add_hex(&mut end, &start, data_size);
                    cmap.add_codespace_range(
                        data_size + 1,
                        hex_to_int(&start, data_size),
                        hex_to_int(&end, data_size),
                    );
                }
            }
            1 => {
                // notdefrange（読み飛ばし）
                stream.read_hex(&mut start, data_size)?;
                stream.read_hex_number(&mut end, data_size)?;
                add_hex(&mut end, &start, data_size);
                let _ = stream.read_number()?;
                for _ in 1..subitems_count {
                    inc_hex(&mut end, data_size);
                    stream.read_hex_number(&mut start, data_size)?;
                    add_hex(&mut start, &end, data_size);
                    stream.read_hex_number(&mut end, data_size)?;
                    add_hex(&mut end, &start, data_size);
                    let _ = stream.read_number()?;
                }
            }
            2 => {
                // cidchar
                stream.read_hex(&mut ch, data_size)?;
                let mut code = stream.read_number()?;
                cmap.map_one(hex_to_int(&ch, data_size), MapValue::Cid(code));
                for _ in 1..subitems_count {
                    inc_hex(&mut ch, data_size);
                    if !sequence {
                        stream.read_hex_number(&mut tmp, data_size)?;
                        add_hex(&mut ch, &tmp, data_size);
                    }
                    // code = signed_delta + (code + 1)
                    let delta = stream.read_signed()?;
                    code = code.wrapping_add(1).wrapping_add_signed(delta);
                    cmap.map_one(hex_to_int(&ch, data_size), MapValue::Cid(code));
                }
            }
            3 => {
                // cidrange
                stream.read_hex(&mut start, data_size)?;
                stream.read_hex_number(&mut end, data_size)?;
                add_hex(&mut end, &start, data_size);
                let mut code = stream.read_number()?;
                cmap.map_cid_range(
                    hex_to_int(&start, data_size),
                    hex_to_int(&end, data_size),
                    code,
                );
                for _ in 1..subitems_count {
                    inc_hex(&mut end, data_size);
                    if !sequence {
                        stream.read_hex_number(&mut start, data_size)?;
                        add_hex(&mut start, &end, data_size);
                    } else {
                        start[..=data_size].copy_from_slice(&end[..=data_size]);
                    }
                    stream.read_hex_number(&mut end, data_size)?;
                    add_hex(&mut end, &start, data_size);
                    code = stream.read_number()?;
                    cmap.map_cid_range(
                        hex_to_int(&start, data_size),
                        hex_to_int(&end, data_size),
                        code,
                    );
                }
            }
            4 => {
                // bfchar（src は 2 バイト固定）
                stream.read_hex(&mut ch, ucs2_data_size)?;
                stream.read_hex(&mut char_code, data_size)?;
                cmap.map_one(
                    hex_to_int(&ch, ucs2_data_size),
                    MapValue::Bytes(hex_to_bytes(&char_code, data_size)),
                );
                for _ in 1..subitems_count {
                    inc_hex(&mut ch, ucs2_data_size);
                    if !sequence {
                        stream.read_hex_number(&mut tmp, ucs2_data_size)?;
                        add_hex(&mut ch, &tmp, ucs2_data_size);
                    }
                    inc_hex(&mut char_code, data_size);
                    stream.read_hex_signed(&mut tmp, data_size)?;
                    add_hex(&mut char_code, &tmp, data_size);
                    cmap.map_one(
                        hex_to_int(&ch, ucs2_data_size),
                        MapValue::Bytes(hex_to_bytes(&char_code, data_size)),
                    );
                }
            }
            5 => {
                // bfrange
                stream.read_hex(&mut start, ucs2_data_size)?;
                stream.read_hex_number(&mut end, ucs2_data_size)?;
                add_hex(&mut end, &start, ucs2_data_size);
                stream.read_hex(&mut char_code, data_size)?;
                cmap.map_bf_range(
                    hex_to_int(&start, ucs2_data_size),
                    hex_to_int(&end, ucs2_data_size),
                    hex_to_bytes(&char_code, data_size),
                );
                for _ in 1..subitems_count {
                    inc_hex(&mut end, ucs2_data_size);
                    if !sequence {
                        stream.read_hex_number(&mut start, ucs2_data_size)?;
                        add_hex(&mut start, &end, ucs2_data_size);
                    } else {
                        start[..=ucs2_data_size].copy_from_slice(&end[..=ucs2_data_size]);
                    }
                    stream.read_hex_number(&mut end, ucs2_data_size)?;
                    add_hex(&mut end, &start, ucs2_data_size);
                    stream.read_hex(&mut char_code, data_size)?;
                    cmap.map_bf_range(
                        hex_to_int(&start, ucs2_data_size),
                        hex_to_int(&end, ucs2_data_size),
                        hex_to_bytes(&char_code, data_size),
                    );
                }
            }
            _ => {
                return Err(BcmapError(format!("unknown bcmap record type: {type_}")));
            }
        }
    }

    Ok(use_cmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_base128() {
        // 0x818407 → 16903 (0x4207) per cmapscompress README
        let mut s = BinaryCMapStream::new(&[0x81, 0x84, 0x07]);
        assert_eq!(s.read_number().unwrap(), 16903);
        // single byte
        let mut s = BinaryCMapStream::new(&[0x00]);
        assert_eq!(s.read_number().unwrap(), 0);
        let mut s = BinaryCMapStream::new(&[0x7f]);
        assert_eq!(s.read_number().unwrap(), 127);
        let mut s = BinaryCMapStream::new(&[0x81, 0x00]);
        assert_eq!(s.read_number().unwrap(), 128);
    }

    #[test]
    fn sn_zigzag() {
        // encode: n>=0 → 2n, n<0 → -2n-1
        // 0 → 0, 1 → 2, -1 → 1, 2 → 4, -2 → 3
        let mut s = BinaryCMapStream::new(&[0x00]);
        assert_eq!(s.read_signed().unwrap(), 0);
        let mut s = BinaryCMapStream::new(&[0x02]);
        assert_eq!(s.read_signed().unwrap(), 1);
        let mut s = BinaryCMapStream::new(&[0x01]);
        assert_eq!(s.read_signed().unwrap(), -1);
        let mut s = BinaryCMapStream::new(&[0x04]);
        assert_eq!(s.read_signed().unwrap(), 2);
        let mut s = BinaryCMapStream::new(&[0x03]);
        assert_eq!(s.read_signed().unwrap(), -2);
    }

    #[test]
    fn ub_fixed_width() {
        // UN value 1 expanded to 2 bytes (size=1) → [0x00, 0x01]
        // encoding of 1 as UN: 0x01
        let mut s = BinaryCMapStream::new(&[0x01]);
        let mut num = [0u8; 16];
        s.read_hex_number(&mut num, 1).unwrap();
        assert_eq!(num[0], 0x00);
        assert_eq!(num[1], 0x01);
    }

    #[test]
    fn sb_fixed_width() {
        // signed +1 as SB[1]: zigzag encode 1 → 2, as 2-byte UB
        // UN for 2: 0x02 → bytes after zigzag decode
        let mut s = BinaryCMapStream::new(&[0x02]);
        let mut num = [0u8; 16];
        s.read_hex_signed(&mut num, 1).unwrap();
        // zigzag of value whose low bit is 0 means positive; 0x0002 >> 1 = 1
        assert_eq!(hex_to_int(&num, 1), 1);
    }

    #[test]
    fn read_string_primitive() {
        // len=3, chars 'A','B','C' = 65,66,67
        let mut s = BinaryCMapStream::new(&[0x03, 65, 66, 67]);
        assert_eq!(s.read_string().unwrap(), "ABC");
    }
}
