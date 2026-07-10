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

/// 字句トークン
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) enum Token {
    Int(i64),
    Real(f64),
    /// リテラル文字列または16進文字列の生バイト
    Str(Vec<u8>),
    Name(String),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    BraceStart,
    BraceEnd,
    /// `obj` `R` `stream` 等、および単独の `>`
    /// 既知演算子は `Cow::Borrowed`（確保なし）、未知のみ `Cow::Owned`
    Cmd(Cow<'static, str>),
    Eof,
}

/// `&[u8]` 上を走るトークナイザ
#[allow(dead_code)]
pub(crate) struct Lexer<'a> {
    data: &'a [u8],
    /// 次に読む位置
    pos: usize,
    /// 現在文字（EOF は -1）
    current: i32,
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
            current: -1,
        };
        lex.next_char();
        lex
    }

    /// 現在文字のバイト位置（EOF 時は data 末尾）
    pub fn byte_pos(&self) -> usize {
        if self.current >= 0 && self.pos > 0 {
            self.pos - 1
        } else {
            self.pos
        }
    }

    /// 指定位置へシークし、その位置の文字を current に載せる
    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
        self.current = -1;
        self.next_char();
    }

    /// stream キーワード直後の改行（CRLF / LF）まで進める
    pub fn skip_to_next_line(&mut self) {
        let mut ch = self.current;
        while ch >= 0 {
            if ch == 0x0d {
                ch = self.next_char();
                if ch == 0x0a {
                    self.next_char();
                }
                break;
            } else if ch == 0x0a {
                self.next_char();
                break;
            }
            ch = self.next_char();
        }
    }

    fn next_char(&mut self) -> i32 {
        if self.pos < self.data.len() {
            let b = self.data[self.pos] as i32;
            self.pos += 1;
            self.current = b;
        } else {
            self.current = -1;
        }
        self.current
    }

    fn peek_char(&self) -> i32 {
        if self.pos < self.data.len() {
            self.data[self.pos] as i32
        } else {
            -1
        }
    }

    /// 次のトークンを返す
    pub fn next_token(&mut self) -> Token {
        // 空白とコメントを読み飛ばす
        let mut comment = false;
        let mut ch = self.current;
        loop {
            if ch < 0 {
                return Token::Eof;
            }
            if comment {
                if ch == 0x0a || ch == 0x0d {
                    comment = false;
                }
            } else if ch == 0x25 {
                // '%'
                comment = true;
            } else if ch >= 0 && ch < 256 && SPECIAL_CHARS[ch as usize] != 1 {
                break;
            }
            ch = self.next_char();
        }

        match ch {
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
                ch = self.next_char();
                if ch == 0x3c {
                    self.next_char();
                    Token::DictStart
                } else {
                    self.get_hex_string()
                }
            }
            0x3e => {
                // '>'
                ch = self.next_char();
                if ch == 0x3e {
                    self.next_char();
                    Token::DictEnd
                } else {
                    Token::Cmd(Cow::Borrowed(">"))
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
                Token::Cmd(Cow::Borrowed(")"))
            }
            _ => self.get_command(ch),
        }
    }

    fn get_number(&mut self) -> Token {
        let mut ch = self.current;
        // 0 以外なら実数。小数点後の桁位置を表す分母
        let mut divide_by: f64 = 0.0;
        let mut sign: f64 = 1.0;

        if ch == 0x2d {
            // '-'
            sign = -1.0;
            ch = self.next_char();
            if ch == 0x2d {
                // 二重マイナスは無視（Adobe 互換）
                ch = self.next_char();
            }
        } else if ch == 0x2b {
            // '+'
            ch = self.next_char();
        }

        // 符号直後の改行は無視
        if ch == 0x0a || ch == 0x0d {
            loop {
                ch = self.next_char();
                if ch != 0x0a && ch != 0x0d {
                    break;
                }
            }
        }

        if ch == 0x2e {
            // '.' 始まり → 次桁は 1/10 の位
            divide_by = 10.0;
            ch = self.next_char();
        }

        if !(0x30..=0x39).contains(&ch) {
            // 数字が続かない場合は 0（Adobe 互換の救済）
            return Token::Int(0);
        }

        let mut base_value: f64 = (ch - 0x30) as f64;

        loop {
            ch = self.next_char();
            if ch < 0 {
                break;
            }
            if (0x30..=0x39).contains(&ch) {
                let digit = (ch - 0x30) as f64;
                if divide_by != 0.0 {
                    divide_by *= 10.0;
                }
                base_value = base_value * 10.0 + digit;
            } else if ch == 0x2e {
                if divide_by == 0.0 {
                    // 整数部の後の小数点（次の桁から divide_by を 10 倍していく）
                    divide_by = 1.0;
                } else {
                    // 二重ドットで打ち切り
                    break;
                }
            } else if ch == 0x2d {
                // 数値中間のマイナスは無視
            } else {
                break;
            }
        }

        if divide_by != 0.0 {
            Token::Real(sign * base_value / divide_by)
        } else {
            Token::Int((sign * base_value) as i64)
        }
    }

    fn get_string(&mut self) -> Token {
        let mut num_paren = 1;
        let mut buf = Vec::new();
        let mut ch = self.next_char();

        loop {
            let mut char_buffered = false;
            match ch {
                -1 => break, // 未終端でも読めた分を返す
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
                    match ch {
                        -1 => break,
                        0x6e => buf.push(b'\n'), // n
                        0x72 => buf.push(b'\r'), // r
                        0x74 => buf.push(b'\t'), // t
                        0x62 => buf.push(0x08),  // b
                        0x66 => buf.push(0x0c),  // f
                        0x5c | 0x28 | 0x29 => buf.push(ch as u8),
                        0x30..=0x37 => {
                            // 8進 \ddd（最大3桁）
                            let mut x = (ch & 0x0f) as u8;
                            ch = self.next_char();
                            char_buffered = true;
                            if (0x30..=0x37).contains(&ch) {
                                x = (x << 3) + ((ch & 0x0f) as u8);
                                ch = self.next_char();
                                if (0x30..=0x37).contains(&ch) {
                                    char_buffered = false;
                                    x = (x << 3) + ((ch & 0x0f) as u8);
                                }
                            }
                            buf.push(x);
                        }
                        0x0d => {
                            // 行継続 CR、CRLF なら LF も消費
                            if self.peek_char() == 0x0a {
                                self.next_char();
                            }
                        }
                        0x0a => {
                            // 行継続 LF
                        }
                        _ => buf.push(ch as u8),
                    }
                }
                _ => buf.push(ch as u8),
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
            let ch = self.next_char();
            if ch < 0 || (ch < 256 && SPECIAL_CHARS[ch as usize] != 0) {
                break;
            }
            if ch == 0x23 {
                // '#'
                let ch1 = self.next_char();
                if ch1 < 0 || (ch1 < 256 && SPECIAL_CHARS[ch1 as usize] != 0) {
                    buf.push(b'#');
                    break;
                }
                let x = to_hex_digit(ch1);
                if x >= 0 {
                    let ch2 = self.next_char();
                    let x2 = to_hex_digit(ch2);
                    if x2 < 0 {
                        buf.push(b'#');
                        buf.push(ch1 as u8);
                        if ch2 < 0 || (ch2 < 256 && SPECIAL_CHARS[ch2 as usize] != 0) {
                            break;
                        }
                        buf.push(ch2 as u8);
                        continue;
                    }
                    buf.push(((x as u8) << 4) | (x2 as u8));
                } else {
                    buf.push(b'#');
                    buf.push(ch1 as u8);
                }
            } else {
                buf.push(ch as u8);
            }
        }
        // バイト列を Latin-1 相当で String 化（各バイトを Unicode スカラーに）
        let name: String = buf.iter().map(|&b| b as char).collect();
        Token::Name(name)
    }

    fn get_hex_string(&mut self) -> Token {
        let mut buf = Vec::new();
        let mut ch = self.current;
        let mut first_digit: i32 = -1;

        loop {
            if ch < 0 {
                break; // 未終端でも読めた分を返す
            } else if ch == 0x3e {
                // '>'
                self.next_char();
                break;
            } else if ch < 256 && SPECIAL_CHARS[ch as usize] == 1 {
                ch = self.next_char();
                continue;
            } else {
                let digit = to_hex_digit(ch);
                if digit < 0 {
                    // 不正文字は無視
                } else if first_digit < 0 {
                    first_digit = digit;
                } else {
                    buf.push(((first_digit as u8) << 4) | (digit as u8));
                    first_digit = -1;
                }
                ch = self.next_char();
            }
        }

        // 奇数桁は末尾 0 補完
        if first_digit >= 0 {
            buf.push((first_digit as u8) << 4);
        }
        Token::Str(buf)
    }

    fn get_command(&mut self, first: i32) -> Token {
        // スタック上の固定長バッファ（上限128は従来の vec 打ち切りと同じ）
        let mut buf = [0u8; 128];
        let mut len = 1usize;
        buf[0] = first as u8;

        // 非表示 ASCII 開始で次が通常文字なら1文字コマンド
        if first < 0x20 || first > 0x7f {
            let next = self.peek_char();
            if (0x20..=0x7f).contains(&next) {
                self.next_char();
                return Token::Cmd(intern_cmd(&buf[..len]));
            }
        }

        loop {
            let ch = self.next_char();
            if ch < 0 || (ch < 256 && SPECIAL_CHARS[ch as usize] != 0) {
                break;
            }
            if len >= 128 {
                break;
            }
            buf[len] = ch as u8;
            len += 1;
        }

        // pdf.js は true/false/null をここでリテラル化するが、
        // トークン種としては Cmd のまま返し Parser 側で解釈する
        Token::Cmd(intern_cmd(&buf[..len]))
    }
}

fn to_hex_digit(ch: i32) -> i32 {
    if (0x30..=0x39).contains(&ch) {
        ch & 0x0f
    } else if (0x41..=0x46).contains(&ch) || (0x61..=0x66).contains(&ch) {
        (ch & 0x0f) + 9
    } else {
        -1
    }
}

fn bytes_to_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// 既知演算子バイト列を `'static` に対応付け、未知は Latin-1 所有文字列にする。
/// 集合は現状 `Token::Cmd` として返るキーワード・コンテンツ演算子を網羅する
///（未知も従来どおり同じ文字列内容の `Cmd` になる）。
fn intern_cmd(bytes: &[u8]) -> Cow<'static, str> {
    match bytes {
        // 構文・オブジェクト
        b"obj" => Cow::Borrowed("obj"),
        b"endobj" => Cow::Borrowed("endobj"),
        b"stream" => Cow::Borrowed("stream"),
        b"endstream" => Cow::Borrowed("endstream"),
        b"xref" => Cow::Borrowed("xref"),
        b"trailer" => Cow::Borrowed("trailer"),
        b"startxref" => Cow::Borrowed("startxref"),
        b"R" => Cow::Borrowed("R"),
        b"true" => Cow::Borrowed("true"),
        b"false" => Cow::Borrowed("false"),
        b"null" => Cow::Borrowed("null"),
        b"f" => Cow::Borrowed("f"),
        b"n" => Cow::Borrowed("n"),
        // インライン画像
        b"BI" => Cow::Borrowed("BI"),
        b"ID" => Cow::Borrowed("ID"),
        b"EI" => Cow::Borrowed("EI"),
        // グラフィックス状態
        b"q" => Cow::Borrowed("q"),
        b"Q" => Cow::Borrowed("Q"),
        b"cm" => Cow::Borrowed("cm"),
        b"w" => Cow::Borrowed("w"),
        b"J" => Cow::Borrowed("J"),
        b"j" => Cow::Borrowed("j"),
        b"M" => Cow::Borrowed("M"),
        b"d" => Cow::Borrowed("d"),
        b"ri" => Cow::Borrowed("ri"),
        b"i" => Cow::Borrowed("i"),
        b"gs" => Cow::Borrowed("gs"),
        // パス構築
        b"m" => Cow::Borrowed("m"),
        b"l" => Cow::Borrowed("l"),
        b"c" => Cow::Borrowed("c"),
        b"v" => Cow::Borrowed("v"),
        b"y" => Cow::Borrowed("y"),
        b"h" => Cow::Borrowed("h"),
        b"re" => Cow::Borrowed("re"),
        // パス描画
        b"S" => Cow::Borrowed("S"),
        b"s" => Cow::Borrowed("s"),
        b"F" => Cow::Borrowed("F"),
        b"f*" => Cow::Borrowed("f*"),
        b"B" => Cow::Borrowed("B"),
        b"B*" => Cow::Borrowed("B*"),
        b"b" => Cow::Borrowed("b"),
        b"b*" => Cow::Borrowed("b*"),
        // クリップ
        b"W" => Cow::Borrowed("W"),
        b"W*" => Cow::Borrowed("W*"),
        // 色空間・色
        b"CS" => Cow::Borrowed("CS"),
        b"cs" => Cow::Borrowed("cs"),
        b"SC" => Cow::Borrowed("SC"),
        b"SCN" => Cow::Borrowed("SCN"),
        b"sc" => Cow::Borrowed("sc"),
        b"scn" => Cow::Borrowed("scn"),
        b"G" => Cow::Borrowed("G"),
        b"g" => Cow::Borrowed("g"),
        b"RG" => Cow::Borrowed("RG"),
        b"rg" => Cow::Borrowed("rg"),
        b"K" => Cow::Borrowed("K"),
        b"k" => Cow::Borrowed("k"),
        // シェーディング・XObject
        b"sh" => Cow::Borrowed("sh"),
        b"Do" => Cow::Borrowed("Do"),
        // インライン画像以外の画像関連は BI/ID/EI に含む
        // テキスト状態・配置・描画
        b"BT" => Cow::Borrowed("BT"),
        b"ET" => Cow::Borrowed("ET"),
        b"Tc" => Cow::Borrowed("Tc"),
        b"Tw" => Cow::Borrowed("Tw"),
        b"Tz" => Cow::Borrowed("Tz"),
        b"TL" => Cow::Borrowed("TL"),
        b"Tf" => Cow::Borrowed("Tf"),
        b"Tr" => Cow::Borrowed("Tr"),
        b"Ts" => Cow::Borrowed("Ts"),
        b"Td" => Cow::Borrowed("Td"),
        b"TD" => Cow::Borrowed("TD"),
        b"Tm" => Cow::Borrowed("Tm"),
        b"T*" => Cow::Borrowed("T*"),
        b"Tj" => Cow::Borrowed("Tj"),
        b"TJ" => Cow::Borrowed("TJ"),
        b"'" => Cow::Borrowed("'"),
        b"\"" => Cow::Borrowed("\""),
        // 互換・マーク付き内容
        b"d0" => Cow::Borrowed("d0"),
        b"d1" => Cow::Borrowed("d1"),
        b"MP" => Cow::Borrowed("MP"),
        b"DP" => Cow::Borrowed("DP"),
        b"BMC" => Cow::Borrowed("BMC"),
        b"BDC" => Cow::Borrowed("BDC"),
        b"EMC" => Cow::Borrowed("EMC"),
        b"BX" => Cow::Borrowed("BX"),
        b"EX" => Cow::Borrowed("EX"),
        // CMap
        b"endcmap" => Cow::Borrowed("endcmap"),
        b"usecmap" => Cow::Borrowed("usecmap"),
        b"begincodespacerange" => Cow::Borrowed("begincodespacerange"),
        b"endcodespacerange" => Cow::Borrowed("endcodespacerange"),
        b"beginbfchar" => Cow::Borrowed("beginbfchar"),
        b"endbfchar" => Cow::Borrowed("endbfchar"),
        b"begincidchar" => Cow::Borrowed("begincidchar"),
        b"endcidchar" => Cow::Borrowed("endcidchar"),
        b"beginbfrange" => Cow::Borrowed("beginbfrange"),
        b"endbfrange" => Cow::Borrowed("endbfrange"),
        b"begincidrange" => Cow::Borrowed("begincidrange"),
        b"endcidrange" => Cow::Borrowed("endcidrange"),
        // 単独区切り救済
        b">" => Cow::Borrowed(">"),
        b")" => Cow::Borrowed(")"),
        _ => Cow::Owned(bytes_to_latin1(bytes)),
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
        assert_eq!(first(b"obj"), Token::Cmd("obj".into()));
        assert_eq!(first(b"R"), Token::Cmd("R".into()));
        assert_eq!(first(b"stream"), Token::Cmd("stream".into()));
        assert_eq!(first(b"endobj"), Token::Cmd("endobj".into()));
        assert_eq!(first(b"true"), Token::Cmd("true".into()));
        assert_eq!(first(b"null"), Token::Cmd("null".into()));
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
                Token::Cmd("obj".into()),
                Token::DictStart,
                Token::Name("Type".into()),
                Token::Name("Page".into()),
                Token::DictEnd,
                Token::Cmd("endobj".into()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn eof_empty() {
        assert_eq!(first(b""), Token::Eof);
    }
}
