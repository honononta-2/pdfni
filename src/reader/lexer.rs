//! PDF字句解析（トークナイザ）
//!
//! 準拠: pdf.js `parser.js` の Lexer（specialChars 表と救済挙動を含む）

use std::borrow::Cow;

/// 空白=1、区切り=2、通常=0（256要素、pdf.js specialChars 相当）
static SPECIAL_CHARS: [u8; 256] = {
    let mut t = [0u8; 256];
    // 空白: NUL TAB LF FF CR SP
    t[0x00] = 1;
    t[0x09] = 1;
    t[0x0a] = 1;
    t[0x0c] = 1;
    t[0x0d] = 1;
    t[0x20] = 1;
    // 区切り: ( ) < > [ ] { } / %
    t[0x28] = 2; // (
    t[0x29] = 2; // )
    t[0x3c] = 2; // <
    t[0x3e] = 2; // >
    t[0x5b] = 2; // [
    t[0x5d] = 2; // ]
    t[0x7b] = 2; // {
    t[0x7d] = 2; // }
    t[0x2f] = 2; // /
    t[0x25] = 2; // %
    t
};

/// 字句コマンド。既知は Op、未知は Latin-1 文字列のまま
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Cmd {
    Op(Op),
    Other(String),
}

/// 既知のキーワード・演算子
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    // 構文・オブジェクト
    Obj,
    EndObj,
    Stream,
    EndStream,
    Xref,
    Trailer,
    StartXref,
    R,
    True,
    False,
    Null,
    LowerF,
    LowerN,
    // インライン画像
    BI,
    ID,
    EI,
    // グラフィックス状態
    LowerQ,
    Q,
    Cm,
    LowerW,
    J,
    LowerJ,
    M,
    LowerD,
    Ri,
    LowerI,
    Gs,
    // パス構築
    LowerM,
    LowerL,
    LowerC,
    LowerV,
    LowerY,
    LowerH,
    Re,
    // パス描画
    S,
    LowerS,
    F,
    LowerFStar,
    B,
    BStar,
    LowerB,
    LowerBStar,
    // クリップ
    W,
    WStar,
    // 色空間・色
    CS,
    Cs,
    SC,
    SCN,
    Sc,
    Scn,
    G,
    LowerG,
    RG,
    Rg,
    K,
    LowerK,
    // シェーディング・XObject
    Sh,
    Do,
    // テキスト
    BT,
    ET,
    Tc,
    Tw,
    Tz,
    TL,
    Tf,
    Tr,
    Ts,
    Td,
    TD,
    Tm,
    TStar,
    Tj,
    TJ,
    Quote,
    DblQuote,
    // 互換・マーク付き内容
    D0,
    D1,
    MP,
    DP,
    BMC,
    BDC,
    EMC,
    BX,
    EX,
    // CMap
    EndCmap,
    UseCmap,
    BeginCodespaceRange,
    EndCodespaceRange,
    BeginBfChar,
    EndBfChar,
    BeginCidChar,
    EndCidChar,
    BeginBfRange,
    EndBfRange,
    BeginCidRange,
    EndCidRange,
    // 単独区切り救済
    Gt,
    CloseParen,
}

/// 字句トークン
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) enum Token {
    Int(i64),
    Real(f64),
    /// リテラル文字列または16進文字列の生バイト
    Str(Vec<u8>),
    Name(Cow<'static, str>),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    BraceStart,
    BraceEnd,
    /// `obj` `R` `stream` 等、および単独の `>`
    /// 既知演算子は `Cmd::Op`、未知のみ `Cmd::Other`
    Cmd(Cmd),
    Eof,
}

/// `&[u8]` 上を走るトークナイザ
#[allow(dead_code)]
pub(crate) struct Lexer<'a> {
    data: &'a [u8],
    /// 次に読む位置
    pos: usize,
    /// 現在文字（EOF は None）
    current: Option<u8>,
}

#[allow(dead_code)]
impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self::new_at(data, 0)
    }

    /// `start` 位置から字句解析を始める
    pub fn new_at(data: &'a [u8], start: usize) -> Self {
        let start = start.min(data.len());
        let mut lex = Self {
            data,
            pos: start,
            current: None,
        };
        lex.next_char();
        lex
    }

    /// 現在文字のバイト位置（EOF 時は data 末尾）
    pub fn byte_pos(&self) -> usize {
        if self.current.is_some() && self.pos > 0 {
            self.pos - 1
        } else {
            self.pos
        }
    }

    /// 指定位置へシークし、その位置の文字を current に載せる
    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
        self.current = None;
        self.next_char();
    }

    /// stream キーワード直後の改行（CRLF / LF）まで進める
    pub fn skip_to_next_line(&mut self) {
        let mut ch = self.current;
        while let Some(b) = ch {
            if b == 0x0d {
                if self.next_char() == Some(0x0a) {
                    self.next_char();
                }
                break;
            } else if b == 0x0a {
                self.next_char();
                break;
            }
            ch = self.next_char();
        }
    }

    fn next_char(&mut self) -> Option<u8> {
        self.current = self.data.get(self.pos).copied();
        if self.current.is_some() {
            self.pos += 1;
        }
        self.current
    }

    fn peek_char(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    /// 次のトークンを返す
    pub fn next_token(&mut self) -> Token {
        // 空白とコメントを読み飛ばす
        let mut comment = false;
        let mut ch = self.current;
        let b = loop {
            let Some(b) = ch else {
                return Token::Eof;
            };
            if comment {
                if b == 0x0a || b == 0x0d {
                    comment = false;
                }
            } else if b == 0x25 {
                // '%'
                comment = true;
            } else if SPECIAL_CHARS[b as usize] != 1 {
                break b;
            }
            ch = self.next_char();
        };

        match b {
            0x30..=0x39 | 0x2b | 0x2d | 0x2e => self.get_number(),
            0x28 => self.get_string(),
            0x2f => self.get_name(),
            0x5b => {
                self.next_char();
                Token::ArrayStart
            }
            0x5d => {
                self.next_char();
                Token::ArrayEnd
            }
            0x3c => {
                // '<'
                if self.next_char() == Some(0x3c) {
                    self.next_char();
                    Token::DictStart
                } else {
                    self.get_hex_string()
                }
            }
            0x3e => {
                // '>'
                if self.next_char() == Some(0x3e) {
                    self.next_char();
                    Token::DictEnd
                } else {
                    Token::Cmd(Cmd::Op(Op::Gt))
                }
            }
            0x7b => {
                self.next_char();
                Token::BraceStart
            }
            0x7d => {
                self.next_char();
                Token::BraceEnd
            }
            0x29 => {
                // 不正な単独 ')'。pdf.js は FormatError だが、ループ固着を避けるため消費する
                self.next_char();
                Token::Cmd(Cmd::Op(Op::CloseParen))
            }
            _ => self.get_command(b),
        }
    }

    /// 数値の読み取り。整数は i64 で正確に累積し、小数点かオーバーフローで f64 へ移行する
    fn get_number(&mut self) -> Token {
        let mut ch = self.current;
        let mut negative = false;

        if ch == Some(0x2d) {
            // '-'
            negative = true;
            ch = self.next_char();
            if ch == Some(0x2d) {
                // 二重マイナスは無視（Adobe 互換）
                ch = self.next_char();
            }
        } else if ch == Some(0x2b) {
            // '+'
            ch = self.next_char();
        }

        // 符号直後の改行は無視
        while matches!(ch, Some(0x0a | 0x0d)) {
            ch = self.next_char();
        }

        let leading_dot = ch == Some(0x2e);
        if leading_dot {
            ch = self.next_char();
        }

        let Some(first @ 0x30..=0x39) = ch else {
            // 数字が続かない場合は 0（Adobe 互換の救済）
            return Token::Int(0);
        };

        let mut int_acc = i64::from(first - 0x30);
        let mut float_acc = 0.0f64;
        // 0 以外なら実数。小数点後の桁位置を表す分母
        let mut divide_by = 0.0f64;
        let mut is_float = false;
        if leading_dot {
            // '.' 始まり → 最初の桁は 1/10 の位
            is_float = true;
            float_acc = f64::from(first - 0x30);
            divide_by = 10.0;
        }

        while let Some(b) = self.next_char() {
            match b {
                0x30..=0x39 => {
                    let digit = b - 0x30;
                    if is_float {
                        if divide_by != 0.0 {
                            divide_by *= 10.0;
                        }
                        float_acc = float_acc * 10.0 + f64::from(digit);
                    } else if let Some(v) = int_acc
                        .checked_mul(10)
                        .and_then(|v| v.checked_add(i64::from(digit)))
                    {
                        int_acc = v;
                    } else {
                        // i64 を超える整数は f64 で継続
                        is_float = true;
                        float_acc = int_acc as f64 * 10.0 + f64::from(digit);
                    }
                }
                0x2e => {
                    if divide_by != 0.0 {
                        // 二重ドットで打ち切り
                        break;
                    }
                    // 整数部の後の小数点（次の桁から divide_by を 10 倍していく）
                    if !is_float {
                        is_float = true;
                        float_acc = int_acc as f64;
                    }
                    divide_by = 1.0;
                }
                // 数値中間のマイナスは無視
                0x2d => {}
                _ => break,
            }
        }

        if !is_float {
            return Token::Int(if negative { -int_acc } else { int_acc });
        }
        let mut v = if divide_by != 0.0 {
            float_acc / divide_by
        } else {
            float_acc
        };
        if negative {
            v = -v;
        }
        if divide_by != 0.0 {
            // 極端に長い小数で Inf/NaN になった場合は 0 に落とす
            Token::Real(if v.is_finite() { v } else { 0.0 })
        } else {
            // i64 を超えた整数は飽和して Int 化
            Token::Int(v as i64)
        }
    }

    fn get_string(&mut self) -> Token {
        let mut num_paren = 1;
        let mut buf = Vec::new();
        let mut ch = self.next_char();

        // EOF は未終端でも読めた分を返す
        while let Some(b) = ch {
            let mut char_buffered = false;
            match b {
                0x28 => {
                    // '('
                    num_paren += 1;
                    buf.push(b'(');
                }
                0x29 => {
                    // ')'
                    num_paren -= 1;
                    if num_paren == 0 {
                        self.next_char();
                        break;
                    }
                    buf.push(b')');
                }
                0x5c => {
                    // '\\'
                    ch = self.next_char();
                    let Some(esc) = ch else { break };
                    match esc {
                        0x6e => buf.push(b'\n'), // n
                        0x72 => buf.push(b'\r'), // r
                        0x74 => buf.push(b'\t'), // t
                        0x62 => buf.push(0x08),  // b
                        0x66 => buf.push(0x0c),  // f
                        0x5c | 0x28 | 0x29 => buf.push(esc),
                        0x30..=0x37 => {
                            // 8進 \ddd（最大3桁）
                            let mut x = esc & 0x0f;
                            ch = self.next_char();
                            char_buffered = true;
                            if let Some(d2 @ 0x30..=0x37) = ch {
                                x = (x << 3) + (d2 & 0x0f);
                                ch = self.next_char();
                                if let Some(d3 @ 0x30..=0x37) = ch {
                                    char_buffered = false;
                                    x = (x << 3) + (d3 & 0x0f);
                                }
                            }
                            buf.push(x);
                        }
                        0x0d => {
                            // 行継続 CR、CRLF なら LF も消費
                            if self.peek_char() == Some(0x0a) {
                                self.next_char();
                            }
                        }
                        0x0a => {
                            // 行継続 LF
                        }
                        _ => buf.push(esc),
                    }
                }
                _ => buf.push(b),
            }
            if !char_buffered {
                ch = self.next_char();
            }
        }
        Token::Str(buf)
    }

    fn get_name(&mut self) -> Token {
        let mut buf = Vec::new();
        loop {
            let Some(b) = self.next_char() else { break };
            if SPECIAL_CHARS[b as usize] != 0 {
                break;
            }
            if b != 0x23 {
                buf.push(b);
                continue;
            }
            // '#' 直後2桁の16進をバイトへ。崩れていれば文字どおりに扱う
            let ch1 = self.next_char();
            let Some(b1) = ch1.filter(|&b1| SPECIAL_CHARS[b1 as usize] == 0) else {
                buf.push(b'#');
                break;
            };
            let Some(x) = to_hex_digit(b1) else {
                buf.push(b'#');
                buf.push(b1);
                continue;
            };
            let ch2 = self.next_char();
            match ch2.and_then(to_hex_digit) {
                Some(x2) => buf.push((x << 4) | x2),
                None => {
                    buf.push(b'#');
                    buf.push(b1);
                    let Some(b2) = ch2.filter(|&b2| SPECIAL_CHARS[b2 as usize] == 0) else {
                        break;
                    };
                    buf.push(b2);
                }
            }
        }
        Token::Name(intern_name(&bytes_to_latin1(&buf)))
    }

    fn get_hex_string(&mut self) -> Token {
        let mut buf = Vec::new();
        let mut ch = self.current;
        let mut first_digit: Option<u8> = None;

        // EOF は未終端でも読めた分を返す
        while let Some(b) = ch {
            if b == 0x3e {
                // '>'
                self.next_char();
                break;
            }
            if SPECIAL_CHARS[b as usize] != 1 {
                // 16進数字以外の不正文字は無視
                match (to_hex_digit(b), first_digit) {
                    (Some(digit), None) => first_digit = Some(digit),
                    (Some(digit), Some(first)) => {
                        buf.push((first << 4) | digit);
                        first_digit = None;
                    }
                    (None, _) => {}
                }
            }
            ch = self.next_char();
        }

        // 奇数桁は末尾 0 補完
        if let Some(first) = first_digit {
            buf.push(first << 4);
        }
        Token::Str(buf)
    }

    fn get_command(&mut self, first: u8) -> Token {
        // スタック上の固定長バッファ（上限128は従来の vec 打ち切りと同じ）
        let mut buf = [0u8; 128];
        let mut len = 1usize;
        buf[0] = first;

        // 非表示 ASCII 開始で次が通常文字なら1文字コマンド
        if !(0x20..=0x7f).contains(&first) && matches!(self.peek_char(), Some(0x20..=0x7f)) {
            self.next_char();
            return Token::Cmd(intern_cmd(&buf[..len]));
        }

        while let Some(b) = self.next_char() {
            if SPECIAL_CHARS[b as usize] != 0 || len >= 128 {
                break;
            }
            buf[len] = b;
            len += 1;
        }

        // pdf.js は true/false/null をここでリテラル化するが、
        // トークン種としては Cmd のまま返し Parser 側で解釈する
        Token::Cmd(intern_cmd(&buf[..len]))
    }
}

fn to_hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b & 0x0f),
        b'A'..=b'F' | b'a'..=b'f' => Some((b & 0x0f) + 9),
        _ => None,
    }
}

fn bytes_to_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// 頻出 PDF Name を `&'static str` に丸めて割り当てを避ける。未知は所有文字列
fn intern_name(s: &str) -> Cow<'static, str> {
    macro_rules! k { ($($name:literal),* $(,)?) => {
        match s { $($name => return Cow::Borrowed($name),)* _ => {} }
    }; }
    k!(
        // 構造・xref・trailer
        "Type", "Subtype", "Root", "Info", "ID", "Prev", "Size", "N", "W",
        "Index", "XRefStm", "Catalog", "Pages", "Page", "Count", "Kids",
        "Parent", "Contents", "MediaBox", "CropBox", "Resources", "Rotate",
        "XRef", "ObjStm", "Metadata", "StructTreeRoot", "Annots", "Annot",
        "Rect", "A", "Name", "F",
        // ストリーム
        "Filter", "FlateDecode", "LZWDecode", "ASCIIHexDecode", "ASCII85Decode",
        "RunLengthDecode", "Crypt", "DecodeParms", "DP", "EarlyChange",
        "BitsPerComponent", "BPC", "Columns", "Predictor", "Length",
        // フォント
        "Font", "FontDescriptor", "BaseFont", "Encoding", "ToUnicode",
        "FirstChar", "LastChar", "Widths", "W2", "DW", "DW2", "FontFile",
        "FontFile2", "FontFile3", "FontMatrix", "FontBBox", "CIDSystemInfo",
        "DescendantFonts", "CIDToGIDMap", "CIDFontType0", "CIDFontType2",
        "TrueType", "Type0", "Type1", "Type3", "MMType1", "Differences",
        "BaseEncoding", "Registry", "Ordering", "Supplement", "Style",
        // FontDescriptor
        "Ascent", "Descent", "CapHeight", "XHeight", "Flags", "FontName",
        "ItalicAngle", "StemV", "StemH", "AvgWidth", "MaxWidth", "MissingWidth",
        "Leading", "FontStretch", "FontWeight", "FontFamily",
        // ストリーム長・ProcSet
        "Length1", "Length2", "Length3", "ProcSet", "PDF", "Text",
        "ImageB", "ImageC", "ImageI",
        // Tf フォント参照名（F0..F31）
        "F0", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9",
        "F10", "F11", "F12", "F13", "F14", "F15", "F16", "F17", "F18", "F19",
        "F20", "F21", "F22", "F23", "F24", "F25", "F26", "F27", "F28", "F29",
        "F30", "F31",
        // Adobe 系リソース名（T1_N・R0..R9・GS0..GS9）
        "T1_0", "T1_1", "T1_2", "T1_3", "T1_4", "T1_5", "T1_6", "T1_7",
        "T1_8", "T1_9",
        "R0", "R1", "R2", "R3", "R4", "R5", "R6", "R7", "R8", "R9",
        "GS0", "GS1", "GS2", "GS3", "GS4", "GS5", "GS6", "GS7", "GS8", "GS9",
        // XObject
        "XObject", "Form", "Image", "BBox", "Matrix",
        // 暗号化
        "Encrypt", "StmF", "StrF", "EFF", "R", "V", "O", "U", "OE", "UE",
        "Perms", "CFM", "AuthEvent", "EncryptMetadata", "CF", "StdCF",
        "Identity", "AESV2", "AESV3", "V2",
    );
    Cow::Owned(s.to_string())
}

/// 既知演算子バイト列を Op に対応付け、未知は Latin-1 所有文字列にする
/// 集合は現状 `Token::Cmd` として返るキーワード・コンテンツ演算子を網羅する
fn intern_cmd(bytes: &[u8]) -> Cmd {
    match bytes {
        // 構文・オブジェクト
        b"obj" => Cmd::Op(Op::Obj),
        b"endobj" => Cmd::Op(Op::EndObj),
        b"stream" => Cmd::Op(Op::Stream),
        b"endstream" => Cmd::Op(Op::EndStream),
        b"xref" => Cmd::Op(Op::Xref),
        b"trailer" => Cmd::Op(Op::Trailer),
        b"startxref" => Cmd::Op(Op::StartXref),
        b"R" => Cmd::Op(Op::R),
        b"true" => Cmd::Op(Op::True),
        b"false" => Cmd::Op(Op::False),
        b"null" => Cmd::Op(Op::Null),
        b"f" => Cmd::Op(Op::LowerF),
        b"n" => Cmd::Op(Op::LowerN),
        // インライン画像
        b"BI" => Cmd::Op(Op::BI),
        b"ID" => Cmd::Op(Op::ID),
        b"EI" => Cmd::Op(Op::EI),
        // グラフィックス状態
        b"q" => Cmd::Op(Op::LowerQ),
        b"Q" => Cmd::Op(Op::Q),
        b"cm" => Cmd::Op(Op::Cm),
        b"w" => Cmd::Op(Op::LowerW),
        b"J" => Cmd::Op(Op::J),
        b"j" => Cmd::Op(Op::LowerJ),
        b"M" => Cmd::Op(Op::M),
        b"d" => Cmd::Op(Op::LowerD),
        b"ri" => Cmd::Op(Op::Ri),
        b"i" => Cmd::Op(Op::LowerI),
        b"gs" => Cmd::Op(Op::Gs),
        // パス構築
        b"m" => Cmd::Op(Op::LowerM),
        b"l" => Cmd::Op(Op::LowerL),
        b"c" => Cmd::Op(Op::LowerC),
        b"v" => Cmd::Op(Op::LowerV),
        b"y" => Cmd::Op(Op::LowerY),
        b"h" => Cmd::Op(Op::LowerH),
        b"re" => Cmd::Op(Op::Re),
        // パス描画
        b"S" => Cmd::Op(Op::S),
        b"s" => Cmd::Op(Op::LowerS),
        b"F" => Cmd::Op(Op::F),
        b"f*" => Cmd::Op(Op::LowerFStar),
        b"B" => Cmd::Op(Op::B),
        b"B*" => Cmd::Op(Op::BStar),
        b"b" => Cmd::Op(Op::LowerB),
        b"b*" => Cmd::Op(Op::LowerBStar),
        // クリップ
        b"W" => Cmd::Op(Op::W),
        b"W*" => Cmd::Op(Op::WStar),
        // 色空間・色
        b"CS" => Cmd::Op(Op::CS),
        b"cs" => Cmd::Op(Op::Cs),
        b"SC" => Cmd::Op(Op::SC),
        b"SCN" => Cmd::Op(Op::SCN),
        b"sc" => Cmd::Op(Op::Sc),
        b"scn" => Cmd::Op(Op::Scn),
        b"G" => Cmd::Op(Op::G),
        b"g" => Cmd::Op(Op::LowerG),
        b"RG" => Cmd::Op(Op::RG),
        b"rg" => Cmd::Op(Op::Rg),
        b"K" => Cmd::Op(Op::K),
        b"k" => Cmd::Op(Op::LowerK),
        // シェーディング・XObject
        b"sh" => Cmd::Op(Op::Sh),
        b"Do" => Cmd::Op(Op::Do),
        // インライン画像以外の画像関連は BI/ID/EI に含む
        // テキスト状態・配置・描画
        b"BT" => Cmd::Op(Op::BT),
        b"ET" => Cmd::Op(Op::ET),
        b"Tc" => Cmd::Op(Op::Tc),
        b"Tw" => Cmd::Op(Op::Tw),
        b"Tz" => Cmd::Op(Op::Tz),
        b"TL" => Cmd::Op(Op::TL),
        b"Tf" => Cmd::Op(Op::Tf),
        b"Tr" => Cmd::Op(Op::Tr),
        b"Ts" => Cmd::Op(Op::Ts),
        b"Td" => Cmd::Op(Op::Td),
        b"TD" => Cmd::Op(Op::TD),
        b"Tm" => Cmd::Op(Op::Tm),
        b"T*" => Cmd::Op(Op::TStar),
        b"Tj" => Cmd::Op(Op::Tj),
        b"TJ" => Cmd::Op(Op::TJ),
        b"'" => Cmd::Op(Op::Quote),
        b"\"" => Cmd::Op(Op::DblQuote),
        // 互換・マーク付き内容
        b"d0" => Cmd::Op(Op::D0),
        b"d1" => Cmd::Op(Op::D1),
        b"MP" => Cmd::Op(Op::MP),
        b"DP" => Cmd::Op(Op::DP),
        b"BMC" => Cmd::Op(Op::BMC),
        b"BDC" => Cmd::Op(Op::BDC),
        b"EMC" => Cmd::Op(Op::EMC),
        b"BX" => Cmd::Op(Op::BX),
        b"EX" => Cmd::Op(Op::EX),
        // CMap
        b"endcmap" => Cmd::Op(Op::EndCmap),
        b"usecmap" => Cmd::Op(Op::UseCmap),
        b"begincodespacerange" => Cmd::Op(Op::BeginCodespaceRange),
        b"endcodespacerange" => Cmd::Op(Op::EndCodespaceRange),
        b"beginbfchar" => Cmd::Op(Op::BeginBfChar),
        b"endbfchar" => Cmd::Op(Op::EndBfChar),
        b"begincidchar" => Cmd::Op(Op::BeginCidChar),
        b"endcidchar" => Cmd::Op(Op::EndCidChar),
        b"beginbfrange" => Cmd::Op(Op::BeginBfRange),
        b"endbfrange" => Cmd::Op(Op::EndBfRange),
        b"begincidrange" => Cmd::Op(Op::BeginCidRange),
        b"endcidrange" => Cmd::Op(Op::EndCidRange),
        // 単独区切り救済
        b">" => Cmd::Op(Op::Gt),
        b")" => Cmd::Op(Op::CloseParen),
        _ => Cmd::Other(bytes_to_latin1(bytes)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &[u8]) -> Vec<Token> {
        let mut lex = Lexer::new(input);
        let mut out = Vec::new();
        loop {
            let t = lex.next_token();
            if t == Token::Eof {
                out.push(t);
                break;
            }
            out.push(t);
        }
        out
    }

    fn first(input: &[u8]) -> Token {
        Lexer::new(input).next_token()
    }

    #[test]
    fn integers() {
        assert_eq!(first(b"42"), Token::Int(42));
        assert_eq!(first(b"-17"), Token::Int(-17));
        assert_eq!(first(b"+9"), Token::Int(9));
        assert_eq!(first(b"0"), Token::Int(0));
    }

    #[test]
    fn reals() {
        assert_eq!(first(b"3.14"), Token::Real(3.14));
        assert_eq!(first(b".5"), Token::Real(0.5));
        assert_eq!(first(b"-0.25"), Token::Real(-0.25));
    }

    #[test]
    fn number_middle_minus_ignored() {
        // "12-34" → 1234（中間マイナス無視）
        assert_eq!(first(b"12-34"), Token::Int(1234));
    }

    #[test]
    fn number_double_dot_truncates() {
        // "1.2.3" → 1.2 で打ち切り、残りは別トークン
        let mut lex = Lexer::new(b"1.2.3");
        assert_eq!(lex.next_token(), Token::Real(1.2));
        // 残り ".3"
        assert_eq!(lex.next_token(), Token::Real(0.3));
    }

    #[test]
    fn literal_string_basic() {
        assert_eq!(first(b"(hello)"), Token::Str(b"hello".to_vec()));
    }

    #[test]
    fn literal_string_escapes() {
        assert_eq!(first(b"(\\n\\r\\t\\b\\f)"), Token::Str(vec![
            b'\n', b'\r', b'\t', 0x08, 0x0c,
        ]));
        assert_eq!(first(b"(\\(\\))"), Token::Str(b"()".to_vec()));
    }

    #[test]
    fn literal_string_octal() {
        // \101 = 'A', \53 = '+', \5 = 5
        assert_eq!(first(b"(\\101\\53\\5)"), Token::Str(vec![b'A', b'+', 5]));
    }

    #[test]
    fn literal_string_nested_parens() {
        assert_eq!(first(b"(a(b)c)"), Token::Str(b"a(b)c".to_vec()));
    }

    #[test]
    fn literal_string_line_continuation() {
        assert_eq!(first(b"(ab\\\ncd)"), Token::Str(b"abcd".to_vec()));
        assert_eq!(first(b"(ab\\\r\ncd)"), Token::Str(b"abcd".to_vec()));
    }

    #[test]
    fn literal_string_unterminated() {
        assert_eq!(first(b"(abc"), Token::Str(b"abc".to_vec()));
    }

    #[test]
    fn hex_string_basic() {
        assert_eq!(first(b"<48656C6C6F>"), Token::Str(b"Hello".to_vec()));
    }

    #[test]
    fn hex_string_odd_digits() {
        // odd trailing digit padded with 0 → 0x40 = '@'
        assert_eq!(first(b"<4>"), Token::Str(vec![0x40]));
    }

    #[test]
    fn hex_string_ignores_invalid() {
        // ignore 'Z', so <4Z1> → digits 4,1 → 0x41 = 'A'
        assert_eq!(first(b"<4Z1>"), Token::Str(vec![0x41]));
    }

    #[test]
    fn name_basic() {
        assert_eq!(first(b"/Type"), Token::Name("Type".into()));
        assert_eq!(first(b"/FlateDecode"), Token::Name("FlateDecode".into()));
    }

    #[test]
    fn name_hex_escape() {
        // /A#20B → "A B"
        assert_eq!(first(b"/A#20B"), Token::Name("A B".into()));
        // /#41 → "A"
        assert_eq!(first(b"/#41"), Token::Name("A".into()));
    }

    #[test]
    fn array_and_dict_delimiters() {
        let ts = tokens(b"[ ] << >> { }");
        assert_eq!(
            &ts[..6],
            &[
                Token::ArrayStart,
                Token::ArrayEnd,
                Token::DictStart,
                Token::DictEnd,
                Token::BraceStart,
                Token::BraceEnd,
            ]
        );
    }

    #[test]
    fn commands() {
        assert_eq!(first(b"obj"), Token::Cmd(Cmd::Op(Op::Obj)));
        assert_eq!(first(b"R"), Token::Cmd(Cmd::Op(Op::R)));
        assert_eq!(first(b"stream"), Token::Cmd(Cmd::Op(Op::Stream)));
        assert_eq!(first(b"endobj"), Token::Cmd(Cmd::Op(Op::EndObj)));
        assert_eq!(first(b"true"), Token::Cmd(Cmd::Op(Op::True)));
        assert_eq!(first(b"null"), Token::Cmd(Cmd::Op(Op::Null)));
    }

    #[test]
    fn skips_comments() {
        let ts = tokens(b"1 % comment here\n2");
        assert_eq!(ts[0], Token::Int(1));
        assert_eq!(ts[1], Token::Int(2));
        assert_eq!(ts[2], Token::Eof);
    }

    #[test]
    fn skips_whitespace() {
        let ts = tokens(b"  \t\n\r 42  ");
        assert_eq!(ts[0], Token::Int(42));
        assert_eq!(ts[1], Token::Eof);
    }

    #[test]
    fn mixed_sequence() {
        let ts = tokens(b"1 0 obj << /Type /Page >> endobj");
        assert_eq!(
            ts,
            vec![
                Token::Int(1),
                Token::Int(0),
                Token::Cmd(Cmd::Op(Op::Obj)),
                Token::DictStart,
                Token::Name("Type".into()),
                Token::Name("Page".into()),
                Token::DictEnd,
                Token::Cmd(Cmd::Op(Op::EndObj)),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn eof_empty() {
        assert_eq!(first(b""), Token::Eof);
    }
}
