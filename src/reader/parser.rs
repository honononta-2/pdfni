//! PDF構文解析とストリーム切り出し
//!
//! 準拠: pdf.js `parser.js` の Parser・makeStream・#findStreamLength

use crate::error::{Error, Result};

use super::lexer::{Lexer, Token};
use super::object::{Dict, Object, Ref, Stream};

/// 間接参照の解決。xref が実装する。/Length が Ref のときに使う
#[allow(dead_code)]
pub(crate) trait Resolver {
    fn resolve(&self, r: Ref) -> Option<Object>;
}

/// 何も解決しない Resolver（テスト・xref 未接続時用）
#[derive(Debug, Default, Clone, Copy)]
#[allow(dead_code)]
pub(crate) struct NullResolver;

impl Resolver for NullResolver {
    fn resolve(&self, _r: Ref) -> Option<Object> {
        None
    }
}

/// "stream" のバイト長（完全一致判定）
const STREAM_SUFFIX_LEN: usize = 6;

/// dict/array のネスト上限。正当な入力を弾かない上限
const MAX_PARSE_DEPTH: usize = 200;

/// 2トークン先読みの Parser
#[allow(dead_code)]
pub(crate) struct Parser<'a> {
    data: &'a [u8],
    lexer: Lexer<'a>,
    buf1: Token,
    buf2: Token,
    depth: usize,
}

#[allow(dead_code)]
impl<'a> Parser<'a> {
    pub fn new(data: &'a [u8], start: usize) -> Self {
        let mut parser = Self {
            data,
            lexer: Lexer::new_at(data, start),
            buf1: Token::Eof,
            buf2: Token::Eof,
            depth: 0,
        };
        parser.refill();
        parser
    }

    fn refill(&mut self) {
        self.buf1 = self.lexer.next_token();
        self.buf2 = self.lexer.next_token();
    }

    fn shift(&mut self) {
        self.buf1 = std::mem::replace(&mut self.buf2, self.lexer.next_token());
    }

    /// 1オブジェクトを構築する
    pub fn get_obj(&mut self, resolver: &dyn Resolver) -> Result<Object> {
        let buf1 = self.buf1.clone();
        self.shift();

        match buf1 {
            Token::DictStart => self.parse_dict(resolver),
            Token::ArrayStart => self.parse_array(resolver),
            Token::Int(n) => {
                if let (Token::Int(g), Token::Cmd(cmd)) = (&self.buf1, &self.buf2) {
                    if cmd == "R" && (0..=i64::from(u32::MAX)).contains(&n) && (0..=i64::from(u16::MAX)).contains(g)
                    {
                        let r = Ref::new(n as u32, *g as u16);
                        self.shift();
                        self.shift();
                        return Ok(Object::Ref(r));
                    }
                }
                Ok(Object::Int(n))
            }
            Token::Real(r) => Ok(Object::Real(r)),
            Token::Str(s) => Ok(Object::Str(s)),
            Token::Name(n) => Ok(Object::Name(n)),
            Token::Cmd(s) => match s.as_ref() {
                "true" => Ok(Object::Bool(true)),
                "false" => Ok(Object::Bool(false)),
                "null" => Ok(Object::Null),
                // 構造以外のコマンドはオブジェクト値としては Null 扱い
                _ => Ok(Object::Null),
            },
            Token::Eof => Ok(Object::Null),
            // 予期しない区切りなどは読み飛ばし相当で Null
            Token::ArrayEnd | Token::DictEnd | Token::BraceStart | Token::BraceEnd => {
                Ok(Object::Null)
            }
        }
    }

    fn parse_dict(&mut self, resolver: &dyn Resolver) -> Result<Object> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Ok(Object::Null);
        }

        let mut dict = Dict::new();
        while !matches!(self.buf1, Token::DictEnd | Token::Eof) {
            let key = match &self.buf1 {
                Token::Name(n) => n.clone(),
                _ => {
                    // キーが Name でないトークンは読み飛ばして継続
                    self.shift();
                    continue;
                }
            };
            self.shift();
            if matches!(self.buf1, Token::Eof) {
                break;
            }
            let value = match self.get_obj(resolver) {
                Ok(v) => v,
                Err(e) => {
                    self.depth -= 1;
                    return Err(e);
                }
            };
            dict.set(key, value);
        }

        if matches!(self.buf1, Token::Eof) {
            // EOF 到達時は読めた分を返す
            self.depth -= 1;
            return Ok(Object::Dict(dict));
        }

        // buf1 は DictEnd。直後が stream ならストリーム化
        if matches!(&self.buf2, Token::Cmd(s) if s == "stream") {
            let result = self.make_stream(dict, resolver);
            self.depth -= 1;
            return result;
        }
        self.shift(); // >>
        self.depth -= 1;
        Ok(Object::Dict(dict))
    }

    fn parse_array(&mut self, resolver: &dyn Resolver) -> Result<Object> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            return Ok(Object::Null);
        }

        let mut arr = Vec::new();
        while !matches!(self.buf1, Token::ArrayEnd | Token::Eof) {
            match self.get_obj(resolver) {
                Ok(v) => arr.push(v),
                Err(e) => {
                    self.depth -= 1;
                    return Err(e);
                }
            }
        }
        if !matches!(self.buf1, Token::Eof) {
            self.shift(); // ]
        }
        // EOF 到達時は読めた分を返す
        self.depth -= 1;
        Ok(Object::Array(arr))
    }

    /// pdf.js makeStream 相当。ストリーム本文のデコードは行わない
    fn make_stream(&mut self, dict: Dict, resolver: &dyn Resolver) -> Result<Object> {
        // buf1 = >> , buf2 = stream。字句位置は stream トークン消費後
        // stream キーワード直後の改行を飛ばし、本文開始位置を得る
        self.lexer.skip_to_next_line();
        let start = self.lexer.byte_pos();

        let mut length = resolve_length(&dict, resolver);
        let max_len = self.data.len().saturating_sub(start);
        if length > max_len {
            length = 0;
        }

        let length = if self.is_endstream_at(start + length) {
            length
        } else {
            self.find_stream_length(start).ok_or_else(|| {
                Error::Reader("Missing endstream command.".into())
            })?
        };

        // endstream（または誤植・切詰）の後へ字句位置を進めてバッファを張り直す
        self.lexer.set_pos(start + length);
        let _ = self.lexer.next_token();
        self.refill();

        Ok(Object::Stream(Stream::new(dict, start, length)))
    }

    /// `pos` から空白を飛ばして `endstream` があるか
    fn is_endstream_at(&self, pos: usize) -> bool {
        if pos > self.data.len() {
            return false;
        }
        let mut lex = Lexer::new_at(self.data, pos);
        matches!(lex.next_token(), Token::Cmd(s) if s == "endstream")
    }

    /// /Length が合わないときの endstream 救済スキャン
    /// 全データがメモリ上にあるためブロック分割はせず末尾まで走査する
    fn find_stream_length(&self, start_pos: usize) -> Option<usize> {
        let data = self.data;
        // 後続候補: stream（正常）/ steam（誤植）/ strea（切詰）
        const PARTIALS: &[&[u8]] = &[b"stream", b"steam", b"strea"];

        if data.len() < start_pos + 3 {
            return None;
        }
        for i in start_pos..=data.len() - 3 {
            if data[i] != b'e' || data[i + 1] != b'n' || data[i + 2] != b'd' {
                continue;
            }
            let after_end = i + 3;
            for part in PARTIALS {
                let mut k = 0;
                while k < part.len()
                    && after_end + k < data.len()
                    && data[after_end + k] == part[k]
                {
                    k += 1;
                }
                if k >= STREAM_SUFFIX_LEN {
                    // endstream
                    return Some(i - start_pos);
                }
                if k == part.len() {
                    // endsteam / endstrea。直後が空白なら採用
                    let next = after_end + k;
                    if next < data.len() && is_endstream_whitespace(data[next]) {
                        return Some(i - start_pos);
                    }
                    break;
                }
            }
        }
        None
    }
}

fn is_endstream_whitespace(b: u8) -> bool {
    matches!(b, 0x20 | 0x09 | 0x0d | 0x0a)
}

/// /Length を解決する。非整数・解決失敗は 0
fn resolve_length(dict: &Dict, resolver: &dyn Resolver) -> usize {
    match dict.get("Length") {
        Some(Object::Int(n)) if *n >= 0 => *n as usize,
        Some(Object::Ref(r)) => match resolver.resolve(*r) {
            Some(Object::Int(n)) if n >= 0 => n as usize,
            _ => 0,
        },
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &[u8]) -> Object {
        let mut p = Parser::new(input, 0);
        p.get_obj(&NullResolver).expect("parse")
    }

    fn parse_with(input: &[u8], resolver: &dyn Resolver) -> Object {
        let mut p = Parser::new(input, 0);
        p.get_obj(resolver).expect("parse")
    }

    #[test]
    fn dict_basic() {
        let obj = parse(b"<< /Type /Page /Count 3 >>");
        let Object::Dict(d) = obj else {
            panic!("expected Dict");
        };
        assert!(matches!(d.get("Type"), Some(Object::Name(n)) if n == "Page"));
        assert!(matches!(d.get("Count"), Some(Object::Int(3))));
    }

    #[test]
    fn array_basic() {
        let obj = parse(b"[1 2 /Foo (bar)]");
        let Object::Array(a) = obj else {
            panic!("expected Array");
        };
        assert_eq!(a.len(), 4);
        assert!(matches!(a[0], Object::Int(1)));
        assert!(matches!(a[1], Object::Int(2)));
        assert!(matches!(&a[2], Object::Name(n) if n == "Foo"));
        assert!(matches!(&a[3], Object::Str(s) if s == b"bar"));
    }

    #[test]
    fn nested_dict_and_array() {
        let obj = parse(b"<< /Kids [1 0 R 2 0 R] /MediaBox [0 0 612 792] >>");
        let Object::Dict(d) = obj else {
            panic!("expected Dict");
        };
        let Object::Array(kids) = d.get("Kids").unwrap() else {
            panic!("expected Kids array");
        };
        assert_eq!(kids.len(), 2);
        assert!(matches!(
            kids[0],
            Object::Ref(Ref {
                num: 1,
                generation: 0
            })
        ));
        assert!(matches!(
            kids[1],
            Object::Ref(Ref {
                num: 2,
                generation: 0
            })
        ));
        let Object::Array(mb) = d.get("MediaBox").unwrap() else {
            panic!("expected MediaBox array");
        };
        assert_eq!(mb.len(), 4);
    }

    #[test]
    fn indirect_ref() {
        let obj = parse(b"1 0 R");
        assert!(matches!(
            obj,
            Object::Ref(Ref {
                num: 1,
                generation: 0
            })
        ));
    }

    #[test]
    fn ref_inside_dict() {
        let obj = parse(b"<< /Parent 3 0 R /Type /Pages >>");
        let Object::Dict(d) = obj else {
            panic!("expected Dict");
        };
        assert!(matches!(
            d.get("Parent"),
            Some(Object::Ref(Ref {
                num: 3,
                generation: 0
            }))
        ));
    }

    #[test]
    fn non_name_dict_key_skipped() {
        // キー位置に整数が来たら読み飛ばし、続く /Type /Page は通常どおり
        let obj = parse(b"<< 42 /Type /Page >>");
        let Object::Dict(d) = obj else {
            panic!("expected Dict");
        };
        assert!(matches!(d.get("Type"), Some(Object::Name(n)) if n == "Page"));
        // 42 をキーとして飛ばしたあと /Type が値側に回る可能性もあるが、
        // pdf.js 同様「キーが Name でないトークンを1つ捨てて継続」する
        // 入力 `42 /Type /Page` では 42 を捨て → /Type がキー、/Page が値
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn array_eof_returns_partial() {
        let obj = parse(b"[1 2 3");
        let Object::Array(a) = obj else {
            panic!("expected Array");
        };
        assert_eq!(a.len(), 3);
        assert!(matches!(a[2], Object::Int(3)));
    }

    #[test]
    fn bool_and_null() {
        let obj = parse(b"[true false null]");
        let Object::Array(a) = obj else {
            panic!("expected Array");
        };
        assert!(matches!(a[0], Object::Bool(true)));
        assert!(matches!(a[1], Object::Bool(false)));
        assert!(matches!(a[2], Object::Null));
    }

    #[test]
    fn stream_with_correct_length() {
        // stream\n + "hello" (5) + \nendstream
        let input = b"<< /Length 5 >>\nstream\nhello\nendstream";
        let obj = parse(input);
        let Object::Stream(s) = obj else {
            panic!("expected Stream, got {obj:?}");
        };
        assert_eq!(s.length, 5);
        assert_eq!(&input[s.start..s.start + s.length], b"hello");
        assert!(matches!(s.dict.get("Length"), Some(Object::Int(5))));
    }

    #[test]
    fn stream_with_crlf_after_stream_keyword() {
        let input = b"<< /Length 3 >>\r\nstream\r\nabc\r\nendstream";
        let obj = parse(input);
        let Object::Stream(s) = obj else {
            panic!("expected Stream, got {obj:?}");
        };
        assert_eq!(s.length, 3);
        assert_eq!(&input[s.start..s.start + s.length], b"abc");
    }

    #[test]
    fn stream_length_wrong_endstream_recovery() {
        // Length は 2 だが本文は "abcdef" (6)
        let input = b"<< /Length 2 >>\nstream\nabcdef\nendstream";
        let obj = parse(input);
        let Object::Stream(s) = obj else {
            panic!("expected Stream, got {obj:?}");
        };
        assert_eq!(&input[s.start..s.start + s.length], b"abcdef\n");
        // endstream の直前まで（改行を含む）
        assert!(
            input[s.start + s.length..].starts_with(b"endstream"),
            "endstream should follow stream data"
        );
    }

    #[test]
    fn stream_length_wrong_steam_recovery() {
        // 誤植 endsteam
        let input = b"<< /Length 0 >>\nstream\nXY\nendsteam\n";
        let obj = parse(input);
        let Object::Stream(s) = obj else {
            panic!("expected Stream, got {obj:?}");
        };
        assert_eq!(&input[s.start..s.start + s.length], b"XY\n");
        assert!(input[s.start + s.length..].starts_with(b"endsteam"));
    }

    #[test]
    fn stream_length_wrong_strea_recovery() {
        // 切詰 endstrea
        let input = b"<< /Length 99 >>\nstream\nZ\nendstrea\n";
        let obj = parse(input);
        let Object::Stream(s) = obj else {
            panic!("expected Stream, got {obj:?}");
        };
        assert_eq!(&input[s.start..s.start + s.length], b"Z\n");
        assert!(input[s.start + s.length..].starts_with(b"endstrea"));
    }

    /// /Length が間接参照のときのテスト用 Resolver
    struct MapResolver {
        num: u32,
        value: i64,
    }

    impl Resolver for MapResolver {
        fn resolve(&self, r: Ref) -> Option<Object> {
            if r.num == self.num {
                Some(Object::Int(self.value))
            } else {
                None
            }
        }
    }

    #[test]
    fn stream_length_indirect_ref() {
        let input = b"<< /Length 10 0 R >>\nstream\nhello\nendstream";
        let resolver = MapResolver {
            num: 10,
            value: 5,
        };
        let obj = parse_with(input, &resolver);
        let Object::Stream(s) = obj else {
            panic!("expected Stream, got {obj:?}");
        };
        assert_eq!(s.length, 5);
        assert_eq!(&input[s.start..s.start + s.length], b"hello");
    }

    #[test]
    fn stream_length_indirect_unresolved_falls_back_to_scan() {
        // NullResolver では Length Ref が解決できず 0 扱い → 救済スキャン
        let input = b"<< /Length 10 0 R >>\nstream\nhello\nendstream";
        let obj = parse(input);
        let Object::Stream(s) = obj else {
            panic!("expected Stream, got {obj:?}");
        };
        assert_eq!(&input[s.start..s.start + s.length], b"hello\n");
    }

    #[test]
    fn stream_missing_endstream_errors() {
        let input = b"<< /Length 5 >>\nstream\nhello";
        let mut p = Parser::new(input, 0);
        let err = p.get_obj(&NullResolver).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("endstream") || msg.contains("Missing"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parser_start_offset() {
        let input = b"garbage << /A 1 >>";
        let mut p = Parser::new(input, 8); // at '<<'
        let obj = p.get_obj(&NullResolver).unwrap();
        let Object::Dict(d) = obj else {
            panic!("expected Dict");
        };
        assert!(matches!(d.get("A"), Some(Object::Int(1))));
    }
}
