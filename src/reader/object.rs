//! PDFオブジェクトモデル

use std::borrow::Cow;
use std::collections::HashMap;

/// PDFオブジェクト参照
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub(crate) struct Ref {
    pub num: u32,
    pub generation: u16,
}

/// PDFの基本型
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) enum Object {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(Cow<'static, str>),
    Array(Vec<Object>),
    Dict(Dict),
    Stream(Stream),
    Ref(Ref),
}

/// 辞書。xref は持たず参照解決は呼び出し側で行う
#[derive(Debug, Clone, Default, PartialEq)]
#[allow(dead_code)]
pub(crate) struct Dict {
    map: HashMap<Cow<'static, str>, Object>,
}

/// ストリーム。生データはバッファ上の範囲として保持しデコードは別単位で行う
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) struct Stream {
    pub dict: Dict,
    /// 生データの開始オフセット
    pub start: usize,
    /// 生データのバイト長
    pub length: usize,
    /// 暗号化オブジェクトの参照（本文復号時に使う）
    pub(crate) crypto_ref: Option<Ref>,
}

#[allow(dead_code)]
impl Ref {
    pub fn new(num: u32, generation: u16) -> Self {
        Self { num, generation }
    }
}

#[allow(dead_code)]
impl Dict {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn set(&mut self, key: impl Into<Cow<'static, str>>, value: Object) {
        self.map.insert(key.into(), value);
    }

    /// 単一キー取得（参照は解決しない）
    pub fn get(&self, key: &str) -> Option<&Object> {
        self.map.get(key)
    }

    /// 2キーのフォールバック取得（略号対応。例: `get2("F", "Filter")`）
    pub fn get2(&self, key1: &str, key2: &str) -> Option<&Object> {
        self.map.get(key1).or_else(|| self.map.get(key2))
    }

    /// 3キーのフォールバック取得
    pub fn get3(&self, key1: &str, key2: &str, key3: &str) -> Option<&Object> {
        self.get2(key1, key2).or_else(|| self.map.get(key3))
    }

    pub fn has(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(|k| k.as_ref())
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Object> {
        self.map.values_mut()
    }

    /// 先頭優先の shallow merge
    pub fn merge(dicts: &[&Dict]) -> Dict {
        let mut out = Dict::new();
        for d in dicts {
            for (k, v) in &d.map {
                out.map
                    .entry(k.clone())
                    .or_insert_with(|| v.clone());
            }
        }
        out
    }
}

#[allow(dead_code)]
impl Stream {
    pub fn new(dict: Dict, start: usize, length: usize) -> Self {
        Self {
            dict,
            start,
            length,
            crypto_ref: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_get2_fallback() {
        let mut d = Dict::new();
        d.set("Filter", Object::Name("FlateDecode".into()));
        assert!(d.get("F").is_none());
        match d.get2("F", "Filter") {
            Some(Object::Name(n)) => assert_eq!(n, "FlateDecode"),
            other => panic!("expected Name, got {other:?}"),
        }
    }

    #[test]
    fn dict_get2_prefers_first_key() {
        let mut d = Dict::new();
        d.set("F", Object::Name("A".into()));
        d.set("Filter", Object::Name("B".into()));
        match d.get2("F", "Filter") {
            Some(Object::Name(n)) => assert_eq!(n, "A"),
            other => panic!("expected Name A, got {other:?}"),
        }
    }

    #[test]
    fn dict_get3_fallback() {
        let mut d = Dict::new();
        d.set("DecodeParms", Object::Int(1));
        match d.get3("DP", "D", "DecodeParms") {
            Some(Object::Int(1)) => {}
            other => panic!("expected Int(1), got {other:?}"),
        }
    }

    #[test]
    fn dict_merge_first_wins() {
        let mut a = Dict::new();
        a.set("Type", Object::Name("Page".into()));
        a.set("Resources", Object::Int(1));
        let mut b = Dict::new();
        b.set("Type", Object::Name("Other".into()));
        b.set("MediaBox", Object::Int(2));
        let m = Dict::merge(&[&a, &b]);
        match m.get("Type") {
            Some(Object::Name(n)) => assert_eq!(n, "Page"),
            other => panic!("expected Page, got {other:?}"),
        }
        assert!(matches!(m.get("Resources"), Some(Object::Int(1))));
        assert!(matches!(m.get("MediaBox"), Some(Object::Int(2))));
    }

    #[test]
    fn object_variants_roundtrip_shape() {
        let r = Ref::new(10, 0);
        let s = Stream::new(Dict::new(), 100, 50);
        assert_eq!(r.num, 10);
        assert_eq!(r.generation, 0);
        assert_eq!(s.start, 100);
        assert_eq!(s.length, 50);
        let _ = Object::Null;
        let _ = Object::Bool(true);
        let _ = Object::Int(42);
        let _ = Object::Real(1.5);
        let _ = Object::Str(b"hi".to_vec());
        let _ = Object::Name("Foo".into());
        let _ = Object::Array(vec![Object::Int(1)]);
        let _ = Object::Dict(Dict::new());
        let _ = Object::Stream(s);
        let _ = Object::Ref(r);
    }
}
