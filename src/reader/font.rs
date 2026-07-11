//! フォント解決とグリフ分解
//!
//! 幅・ToUnicode・ascent/descent。準拠: pdf.js evaluator/fonts、specs/05

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use super::cmap::{utf16be_bytes_to_string, CMap, MapValue};
use super::data::{
    encoding_by_name, font_basic_metrics, glyph_unicode, has_standard14_metrics,
    is_serif_font, is_symbols_font, normalize_font_name, standard14_default_width,
    standard14_glyph_width, STANDARD, WIN_ANSI, MAC_ROMAN, SYMBOL, ZAPF_DINGBATS,
};
use super::filters::decode_stream;
use super::object::{Dict, Object, Ref, Stream};
use super::parser::Resolver;
use super::xref::XRef;

/// 補正済み既定 ascent / descent（em）
const DEFAULT_ASCENT: f64 = 0.88;
const DEFAULT_DESCENT: f64 = -0.12;
/// /Widths も標準14も無いときの暫定幅
const PROVISIONAL_WIDTH: f64 = 500.0;

/// FontDescriptor /Flags
const FLAG_SYMBOLIC: i64 = 4;
const FLAG_NONSYMBOLIC: i64 = 32;

/// 縦メトリクス [w1y, v1x, v1y]（グリフ空間 /1000 単位）
pub(crate) type VMetric = [f64; 3];

/// 読み込み済みフォント
#[derive(Debug, Clone)]
pub(crate) struct LoadedFont {
    /// リソース上の名前
    name: String,
    /// BaseFont 名（空ならリソース名を公開名に使う）
    base_font: String,
    /// charcode または CID → グリフ空間幅（/1000 単位）
    widths: HashMap<u32, f64>,
    default_width: f64,
    /// CID → 縦メトリクス
    vmetrics: HashMap<u32, VMetric>,
    /// 縦書き時の既定 vmetric。横書きは None
    default_vmetric: Option<VMetric>,
    /// charcode → Unicode 文字列
    to_unicode: HashMap<u32, String>,
    /// 複合用符号化 CMap（None なら1バイト）
    encoding_cmap: Option<CMap>,
    /// UCS2 コンパニオン（CID → UTF-16BE）。遅延 lookup 用
    ucs2_cmap: Option<CMap>,
    /// ToUnicode が Identity 相当
    identity_tounicode: bool,
    ascent: f64,
    descent: f64,
}

impl LoadedFont {
    /// 辞書が無いときのフォールバック（幅500・Latin-1・既定 ascent/descent）
    pub fn stub(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            name,
            base_font: String::new(),
            widths: HashMap::new(),
            default_width: PROVISIONAL_WIDTH,
            vmetrics: HashMap::new(),
            default_vmetric: None,
            to_unicode: HashMap::new(),
            encoding_cmap: None,
            ucs2_cmap: None,
            identity_tounicode: false,
            ascent: DEFAULT_ASCENT,
            descent: DEFAULT_DESCENT,
        }
    }

    /// 公開用フォント名（BaseFont 優先、なければロード時のリソース名）
    pub fn display_name(&self) -> &str {
        if self.base_font.is_empty() {
            &self.name
        } else {
            &self.base_font
        }
    }

    /// TextFont.name 用。BaseFont 優先、なければ参照時のリソース名
    pub fn text_font_name<'a>(&'a self, resource_name: &'a str) -> &'a str {
        if self.base_font.is_empty() {
            if resource_name.is_empty() {
                self.display_name()
            } else {
                resource_name
            }
        } else {
            self.display_name()
        }
    }

    /// 文字列バイト列のグリフ数
    pub fn glyph_count(&self, bytes: &[u8]) -> usize {
        if let Some(cmap) = &self.encoding_cmap {
            let mut i = 0;
            let mut n = 0;
            while i < bytes.len() {
                let (_, len) = cmap.read_char_code(bytes, i);
                i += len;
                n += 1;
            }
            n
        } else {
            bytes.len()
        }
    }

    /// 文字列バイト列をグリフごとに訪問する
    ///
    /// 引数: (unicode文字列, グリフ空間の字送り幅(/1000), 元コードが1バイト空白か,
    /// 縦メトリクス, 元 charcode が 0x20 か)
    pub fn for_each_glyph<'a, F>(&'a self, bytes: &[u8], mut visit: F)
    where
        F: FnMut(Cow<'a, str>, f64, bool, Option<VMetric>, bool),
    {
        if let Some(cmap) = &self.encoding_cmap {
            let mut i = 0;
            while i < bytes.len() {
                let (charcode, len) = cmap.read_char_code(bytes, i);
                let is_space = len == 1 && bytes[i] == 0x20;
                i += len;

                let cid = charcode_to_cid(cmap, charcode);
                let width = self
                    .widths
                    .get(&cid)
                    .copied()
                    .unwrap_or(self.default_width);
                let unicode = self.resolve_unicode(charcode, cid, true);
                let vmetric = self.vmetric_for(cid);
                visit(unicode, width, is_space, vmetric, charcode == 0x20);
            }
        } else {
            for &b in bytes {
                let charcode = u32::from(b);
                let width = self
                    .widths
                    .get(&charcode)
                    .copied()
                    .unwrap_or(self.default_width);
                let unicode = self.resolve_unicode(charcode, charcode, false);
                let vmetric = self.vmetric_for(charcode);
                visit(unicode, width, b == 0x20, vmetric, charcode == 0x20);
            }
        }
    }

    /// 文字列バイト列をグリフ列に分解する（テスト用）
    ///
    /// 戻り値: (unicode文字列, グリフ空間の字送り幅(/1000), 元コードが1バイト空白か,
    /// 縦メトリクス, 元 charcode が 0x20 か)
    #[cfg(test)]
    pub fn chars_to_glyphs(
        &self,
        bytes: &[u8],
    ) -> Vec<(Cow<'_, str>, f64, bool, Option<VMetric>, bool)> {
        let mut out = Vec::with_capacity(self.glyph_count(bytes));
        self.for_each_glyph(bytes, |unicode, width, is_space, vmetric, is_20| {
            out.push((unicode, width, is_space, vmetric, is_20));
        });
        out
    }

    /// CID の縦メトリクス（縦書きフォントのみ）
    pub fn vmetric_for(&self, cid: u32) -> Option<VMetric> {
        let default = self.default_vmetric?;
        Some(self.vmetrics.get(&cid).copied().unwrap_or(default))
    }

    fn resolve_unicode(&self, charcode: u32, cid: u32, composite: bool) -> Cow<'_, str> {
        if let Some(s) = self.to_unicode.get(&charcode) {
            return Cow::Borrowed(s.as_str());
        }
        if self.identity_tounicode {
            return Cow::Owned(codepoint_to_string(charcode));
        }
        if composite {
            if let Some(ucs2) = &self.ucs2_cmap {
                if let Some(MapValue::Bytes(b)) = ucs2.lookup(cid).as_deref() {
                    let s = utf16be_bytes_to_string(b);
                    if !s.is_empty() {
                        return Cow::Owned(s);
                    }
                }
            }
            return Cow::Owned(codepoint_to_string(charcode));
        }
        // 単純フォント: Encoding+AGL は load 時に to_unicode へ展開済み
        // ここに来るのは未登録スロット → Latin-1 暫定
        if charcode <= 0xff {
            return Cow::Owned(char::from(charcode as u8).to_string());
        }
        Cow::Owned(codepoint_to_string(charcode))
    }

    /// 符号化 CMap が縦書きか
    pub fn vertical(&self) -> bool {
        self.encoding_cmap
            .as_ref()
            .map(|c| c.vertical())
            .unwrap_or(false)
    }

    /// ベースラインからの上昇（em、補正済み）
    pub fn ascent(&self) -> f64 {
        self.ascent
    }

    /// ベースラインからの下降（em、負値、補正済み）
    pub fn descent(&self) -> f64 {
        self.descent
    }
}

/// charcode → CID（符号化 CMap 経由）
fn charcode_to_cid(cmap: &CMap, charcode: u32) -> u32 {
    match cmap.lookup(charcode).as_deref() {
        Some(MapValue::Cid(cid)) => *cid,
        Some(MapValue::Bytes(b)) => bytes_as_int(b),
        None => charcode,
    }
}

/// resources の /Font から名前で引き、解釈済みフォントを返す
pub(crate) fn lookup_font(
    resources: &Dict,
    name: &str,
    data: &[u8],
    xref: Option<&XRef>,
) -> Arc<LoadedFont> {
    let Some(font_res) = resources.get("Font") else {
        return Arc::new(LoadedFont::stub(name));
    };
    let fonts = match resolve_obj(font_res, data, xref) {
        Object::Dict(d) => d,
        _ => return Arc::new(LoadedFont::stub(name)),
    };
    let Some(entry) = fonts.get(name) else {
        return Arc::new(LoadedFont::stub(name));
    };

    // 間接参照は文書内でキャッシュする
    // 読み込み中はロックを保持しない（読み込みが再帰的にキャッシュへ入る可能性があるため）
    if let (Object::Ref(r), Some(xref)) = (entry, xref) {
        let key = (r.num, r.generation);
        if let Ok(guard) = xref.font_cache.lock() {
            if let Some(cached) = guard.get(&key) {
                return Arc::clone(cached);
            }
        }
        let loaded = Arc::new(load_font_object(entry, name, data, Some(xref)));
        if let Ok(mut guard) = xref.font_cache.lock() {
            // 読み込み中に他スレッドが挿入済みなら既存を返す
            if let Some(cached) = guard.get(&key) {
                return Arc::clone(cached);
            }
            guard.insert(key, Arc::clone(&loaded));
        }
        return loaded;
    }

    Arc::new(load_font_object(entry, name, data, xref))
}

fn load_font_object(
    entry: &Object,
    name: &str,
    data: &[u8],
    xref: Option<&XRef>,
) -> LoadedFont {
    match resolve_obj(entry, data, xref) {
        Object::Dict(dict) => load_font_dict(&dict, name, data, xref),
        _ => LoadedFont::stub(name),
    }
}

fn load_font_dict(dict: &Dict, name: &str, data: &[u8], xref: Option<&XRef>) -> LoadedFont {
    let subtype = dict
        .get("Subtype")
        .and_then(|o| match resolve_obj(o, data, xref) {
            Object::Name(n) => Some(n),
            _ => None,
        })
        .unwrap_or_default();

    match subtype.as_str() {
        "Type0" => load_type0(dict, name, data, xref),
        "Type3" => load_type3(dict, name, data, xref),
        // Type1 / TrueType / MMType1 / CIDFontType* 等は単純として扱う
        // （CIDFont 単体は Type0 経由が本筋）
        _ => load_simple(dict, name, data, xref),
    }
}

/// 単純フォント（Type1 / TrueType）
fn load_simple(dict: &Dict, name: &str, data: &[u8], xref: Option<&XRef>) -> LoadedFont {
    let first_char = dict_int(dict, "FirstChar", data, xref).unwrap_or(0).max(0) as u32;
    let _last_char = dict_int(dict, "LastChar", data, xref).unwrap_or(0xff).max(0) as u32;

    let descriptor = font_descriptor(dict, data, xref);
    let base_font = base_font_name(dict, data, xref);
    let subtype = dict
        .get("Subtype")
        .and_then(|o| match resolve_obj(o, data, xref) {
            Object::Name(n) => Some(n),
            _ => None,
        })
        .unwrap_or_else(|| "Type1".into());
    let flags = descriptor
        .get("Flags")
        .and_then(|o| as_int(&resolve_obj(o, data, xref)))
        .unwrap_or(0);
    let embedded = has_embedded_font(&descriptor, data, xref);

    let (mut default_encoding, differences, base_encoding_name) =
        resolve_simple_encoding(dict, &subtype, flags, &base_font, embedded, data, xref);

    // 埋め込み Type1 の内蔵エンコーディングを既定表に反映
    // （BaseEncoding / 名前付き指定があるときはそちらが優先。Differences は後段で上書き）
    if base_encoding_name.is_none() {
        if let Some(builtin) = type1_builtin_encoding(&descriptor, data, xref) {
            for (code, name) in builtin {
                default_encoding[code as usize] = name;
            }
        }
    }

    let (mut widths, mut default_width) =
        extract_simple_widths(dict, &descriptor, first_char, data, xref);

    // /Widths が無ければ標準14メトリクスから Encoding+Differences 経由で構築
    if dict.get("Widths").is_none() {
        let metrics_name = resolve_metrics_font_name(&base_font);
        if let Some(dw) = standard14_default_width(&metrics_name) {
            widths = build_char_code_to_width(&metrics_name, &default_encoding, &differences);
            default_width = descriptor
                .get("MissingWidth")
                .and_then(|o| as_number(&resolve_obj(o, data, xref)))
                .unwrap_or(dw);
        } else {
            default_width = PROVISIONAL_WIDTH;
            widths.clear();
        }
    }

    let (mut to_unicode, identity_tounicode) = load_tounicode(dict, None, data, xref);
    // ToUnicode が無い、または穴がある場合に Encoding+AGL で埋める
    if !identity_tounicode {
        let simple_tu =
            simple_font_to_unicode(&default_encoding, &differences, base_encoding_name.as_deref());
        if to_unicode.is_empty() {
            to_unicode = simple_tu;
        } else {
            for (cc, s) in simple_tu {
                to_unicode.entry(cc).or_insert(s);
            }
        }
    }

    let (ascent, descent) =
        resolve_ascent_descent(&descriptor, Some(&base_font), data, xref);

    LoadedFont {
        name: name.into(),
        base_font,
        widths,
        default_width,
        vmetrics: HashMap::new(),
        default_vmetric: None,
        to_unicode,
        encoding_cmap: None,
        ucs2_cmap: None,
        identity_tounicode,
        ascent,
        descent,
    }
}

/// Type3: 幅を FontMatrix[0]*1000 で /1000 換算に正規化
fn load_type3(dict: &Dict, name: &str, data: &[u8], xref: Option<&XRef>) -> LoadedFont {
    let first_char = dict_int(dict, "FirstChar", data, xref).unwrap_or(0).max(0) as u32;
    let descriptor = font_descriptor(dict, data, xref);
    let (mut widths, mut default_width) =
        extract_simple_widths(dict, &descriptor, first_char, data, xref);

    let fm0 = font_matrix_a(dict, data, xref);
    // 正規化: width × FontMatrix[0] × 1000
    let scale = fm0 * 1000.0;
    for w in widths.values_mut() {
        *w *= scale;
    }
    default_width *= scale;

    let base_font = base_font_name(dict, data, xref);
    let flags = descriptor
        .get("Flags")
        .and_then(|o| as_int(&resolve_obj(o, data, xref)))
        .unwrap_or(0);
    let (default_encoding, differences, base_encoding_name) =
        resolve_simple_encoding(dict, "Type3", flags, &base_font, false, data, xref);

    let (mut to_unicode, identity_tounicode) = load_tounicode(dict, None, data, xref);
    if !identity_tounicode {
        let simple_tu =
            simple_font_to_unicode(&default_encoding, &differences, base_encoding_name.as_deref());
        if to_unicode.is_empty() {
            to_unicode = simple_tu;
        } else {
            for (cc, s) in simple_tu {
                to_unicode.entry(cc).or_insert(s);
            }
        }
    }
    // Type3 は埋め込み TrueType を持たない
    let (ascent, descent) =
        resolve_ascent_descent(&descriptor, Some(&base_font), data, xref);

    LoadedFont {
        name: name.into(),
        base_font,
        widths,
        default_width,
        vmetrics: HashMap::new(),
        default_vmetric: None,
        to_unicode,
        encoding_cmap: None,
        ucs2_cmap: None,
        identity_tounicode,
        ascent,
        descent,
    }
}

/// 複合フォント Type0
fn load_type0(dict: &Dict, name: &str, data: &[u8], xref: Option<&XRef>) -> LoadedFont {
    // DescendantFonts[0]
    let cid_dict = descendant_font(dict, data, xref);
    let descriptor = font_descriptor(&cid_dict, data, xref);
    let mut base_font = base_font_name(&cid_dict, data, xref);
    if base_font.is_empty() {
        base_font = base_font_name(dict, data, xref);
    }

    let (widths, default_width) = extract_cid_widths(&cid_dict, data, xref);

    let (encoding_cmap, encoding_builtin) = load_encoding_cmap(dict, data, xref);
    let (to_unicode, identity_tounicode) = load_tounicode(dict, Some(dict), data, xref);

    // ToUnicode が無いとき UCS2 コンパニオンを遅延 lookup 用に保持
    let ucs2_cmap = if !identity_tounicode && to_unicode.is_empty() {
        load_ucs2_companion(&encoding_cmap, encoding_builtin, &cid_dict, data, xref)
    } else {
        None
    };

    let (vmetrics, default_vmetric) = if encoding_cmap.vertical() {
        extract_cid_vmetrics(&cid_dict, default_width, data, xref)
    } else {
        (HashMap::new(), None)
    };

    let (ascent, descent) =
        resolve_ascent_descent(&descriptor, Some(&base_font), data, xref);

    LoadedFont {
        name: name.into(),
        base_font,
        widths,
        default_width,
        vmetrics,
        default_vmetric,
        to_unicode,
        encoding_cmap: Some(encoding_cmap),
        ucs2_cmap,
        identity_tounicode,
        ascent,
        descent,
    }
}

fn descendant_font(type0: &Dict, data: &[u8], xref: Option<&XRef>) -> Dict {
    let Some(df) = type0.get("DescendantFonts") else {
        return Dict::new();
    };
    let arr = match resolve_obj(df, data, xref) {
        Object::Array(a) => a,
        _ => return Dict::new(),
    };
    let Some(first) = arr.first() else {
        return Dict::new();
    };
    match resolve_obj(first, data, xref) {
        Object::Dict(d) => d,
        _ => Dict::new(),
    }
}

fn font_descriptor(dict: &Dict, data: &[u8], xref: Option<&XRef>) -> Dict {
    match dict.get("FontDescriptor") {
        Some(o) => match resolve_obj(o, data, xref) {
            Object::Dict(d) => d,
            _ => Dict::new(),
        },
        None => Dict::new(),
    }
}

fn base_font_name(dict: &Dict, data: &[u8], xref: Option<&XRef>) -> String {
    match dict.get("BaseFont") {
        Some(o) => match resolve_obj(o, data, xref) {
            Object::Name(n) => n,
            // BaseFont が文字列 → Name 化（救済 #7）
            Object::Str(s) => String::from_utf8_lossy(&s).into_owned(),
            _ => String::new(),
        },
        None => String::new(),
    }
}

/// 単純フォントの /Widths + MissingWidth
fn extract_simple_widths(
    dict: &Dict,
    descriptor: &Dict,
    first_char: u32,
    data: &[u8],
    xref: Option<&XRef>,
) -> (HashMap<u32, f64>, f64) {
    let mut widths = HashMap::new();
    let mut default_width = 0.0;

    if let Some(wobj) = dict.get("Widths") {
        if let Object::Array(arr) = resolve_obj(wobj, data, xref) {
            let mut j = first_char;
            for w in &arr {
                let resolved = resolve_obj(w, data, xref);
                if let Some(n) = as_number(&resolved) {
                    widths.insert(j, n);
                }
                // 非数値はスキップ（穴あき。j は進める）
                j = j.saturating_add(1);
            }
            default_width = descriptor
                .get("MissingWidth")
                .and_then(|o| as_number(&resolve_obj(o, data, xref)))
                .unwrap_or(0.0);
        }
    }

    (widths, default_width)
}

/// CID フォントの /W（2形式混在）と /DW
fn extract_cid_widths(
    cid_dict: &Dict,
    data: &[u8],
    xref: Option<&XRef>,
) -> (HashMap<u32, f64>, f64) {
    let default_width = cid_dict
        .get("DW")
        .and_then(|o| as_number(&resolve_obj(o, data, xref)))
        .map(|n| n.ceil())
        .unwrap_or(1000.0);

    let mut widths = HashMap::new();
    let Some(wobj) = cid_dict.get("W") else {
        return (widths, default_width);
    };
    let Object::Array(arr) = resolve_obj(wobj, data, xref) else {
        return (widths, default_width);
    };

    let mut i = 0;
    while i < arr.len() {
        let start_obj = resolve_obj(&arr[i], data, xref);
        let Some(start) = as_int(&start_obj) else {
            break; // 不正で break
        };
        i += 1;
        if i >= arr.len() {
            break;
        }
        let next = resolve_obj(&arr[i], data, xref);
        if let Object::Array(ws) = next {
            // c [w1 w2 ...]
            let mut c = start.max(0) as u32;
            for w in &ws {
                if let Some(n) = as_number(&resolve_obj(w, data, xref)) {
                    widths.insert(c, n);
                }
                c = c.saturating_add(1);
            }
            i += 1;
        } else if let Some(end) = as_int(&next) {
            // c1 c2 w
            i += 1;
            if i >= arr.len() {
                break;
            }
            let w_obj = resolve_obj(&arr[i], data, xref);
            let Some(w) = as_number(&w_obj) else {
                // 非数値は continue（このエントリを飛ばす）
                continue;
            };
            i += 1;
            let s = start.max(0) as u32;
            let e = end.max(0) as u32;
            // 正当な入力を弾かない上限: CID は 16bit で 65535 超は参照されない
            let e = e.min(65535);
            if s <= e {
                for c in s..=e {
                    widths.insert(c, w);
                }
            }
        } else {
            break;
        }
    }

    (widths, default_width)
}

/// CID フォントの /DW2・/W2（pdf.js と同形式）
///
/// 内部 vmetric は [w1y, v1x, v1y]。既定は [-1000, DW/2, 880]
fn extract_cid_vmetrics(
    cid_dict: &Dict,
    default_width: f64,
    data: &[u8],
    xref: Option<&XRef>,
) -> (HashMap<u32, VMetric>, Option<VMetric>) {
    let (vy, w1y) = match cid_dict.get("DW2") {
        Some(o) => match resolve_obj(o, data, xref) {
            Object::Array(arr) if arr.len() == 2 => {
                let a = as_number(&resolve_obj(&arr[0], data, xref));
                let b = as_number(&resolve_obj(&arr[1], data, xref));
                match (a, b) {
                    (Some(a), Some(b)) => (a, b),
                    _ => (880.0, -1000.0),
                }
            }
            _ => (880.0, -1000.0),
        },
        None => (880.0, -1000.0),
    };
    let default_vmetric: VMetric = [w1y, default_width * 0.5, vy];

    let mut vmetrics = HashMap::new();
    let Some(w2obj) = cid_dict.get("W2") else {
        return (vmetrics, Some(default_vmetric));
    };
    let Object::Array(arr) = resolve_obj(w2obj, data, xref) else {
        return (vmetrics, Some(default_vmetric));
    };

    let mut i = 0;
    while i < arr.len() {
        let start_obj = resolve_obj(&arr[i], data, xref);
        let Some(start) = as_int(&start_obj) else {
            break;
        };
        i += 1;
        if i >= arr.len() {
            break;
        }
        let next = resolve_obj(&arr[i], data, xref);
        if let Object::Array(vals) = next {
            // c [w1y v1x v1y w1y v1x v1y ...]
            let mut c = start.max(0) as u32;
            let mut j = 0;
            while j + 2 < vals.len() {
                let w1 = as_number(&resolve_obj(&vals[j], data, xref));
                let vx = as_number(&resolve_obj(&vals[j + 1], data, xref));
                let vy = as_number(&resolve_obj(&vals[j + 2], data, xref));
                if let (Some(w1), Some(vx), Some(vy)) = (w1, vx, vy) {
                    vmetrics.insert(c, [w1, vx, vy]);
                }
                c = c.saturating_add(1);
                j += 3;
            }
            i += 1;
        } else if let Some(end) = as_int(&next) {
            // c1 c2 w1y v1x v1y
            i += 1;
            if i + 2 >= arr.len() {
                break;
            }
            let w1 = as_number(&resolve_obj(&arr[i], data, xref));
            let vx = as_number(&resolve_obj(&arr[i + 1], data, xref));
            let vy = as_number(&resolve_obj(&arr[i + 2], data, xref));
            i += 3;
            let Some(w1) = w1 else { continue };
            let Some(vx) = vx else { continue };
            let Some(vy) = vy else { continue };
            let s = start.max(0) as u32;
            let e = end.max(0) as u32;
            // 正当な入力を弾かない上限: CID は 16bit で 65535 超は参照されない
            let e = e.min(65535);
            if s <= e {
                for c in s..=e {
                    vmetrics.insert(c, [w1, vx, vy]);
                }
            }
        } else {
            break;
        }
    }

    (vmetrics, Some(default_vmetric))
}

/// /Encoding を CMap に解決する。戻り値の bool は定義済み（builtin）か
fn load_encoding_cmap(type0: &Dict, data: &[u8], xref: Option<&XRef>) -> (CMap, bool) {
    let Some(enc) = type0.get("Encoding") else {
        eprintln!("reader: Type0 font missing /Encoding; treating as Identity-H");
        return (CMap::identity_h(), true);
    };
    match resolve_obj(enc, data, xref) {
        Object::Name(n) => {
            if let Some(c) = CMap::from_identity_name(&n) {
                return (c, true);
            }
            match load_predefined_cmap(&n, xref) {
                Ok(c) => (c, true),
                Err(e) => {
                    eprintln!(
                        "reader: failed to load predefined CMap '{n}' ({e}); treating as Identity-H"
                    );
                    (CMap::identity_h(), true)
                }
            }
        }
        Object::Stream(s) => {
            let decoded = decode_stream_bytes(data, &s, xref).unwrap_or_default();
            (CMap::parse_embedded(&decoded), false)
        }
        _ => {
            eprintln!("reader: invalid /Encoding; treating as Identity-H");
            (CMap::identity_h(), true)
        }
    }
}

/// UCS2 コンパニオン CMap を条件に応じてロードする
fn load_ucs2_companion(
    encoding_cmap: &CMap,
    encoding_builtin: bool,
    cid_dict: &Dict,
    data: &[u8],
    xref: Option<&XRef>,
) -> Option<CMap> {
    let cid_info = read_cid_system_info(cid_dict, data, xref);
    let use_ucs2 = (encoding_builtin && !encoding_cmap.is_identity())
        || cid_info
            .as_ref()
            .is_some_and(|(reg, ord)| reg == "Adobe" && is_cjk_ordering(ord));
    if !use_ucs2 {
        return None;
    }
    let (registry, ordering) = cid_info?;
    let name = format!("{registry}-{ordering}-UCS2");
    match load_predefined_cmap(&name, xref) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("reader: failed to load UCS2 CMap '{name}' ({e})");
            None
        }
    }
}

/// 定義済み CMap をロード。xref があれば文書単位キャッシュを使う
fn load_predefined_cmap(name: &str, xref: Option<&XRef>) -> crate::error::Result<CMap> {
    let cache = xref.map(|x| x.cmap_cache.as_ref());
    let arc = CMap::load_predefined_shared(name, cache)?;
    Ok(Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone()))
}

fn is_cjk_ordering(ordering: &str) -> bool {
    matches!(ordering, "GB1" | "CNS1" | "Japan1" | "Korea1")
}

/// CIDSystemInfo から Registry / Ordering を読む
fn read_cid_system_info(
    cid_dict: &Dict,
    data: &[u8],
    xref: Option<&XRef>,
) -> Option<(String, String)> {
    let info = cid_dict.get("CIDSystemInfo")?;
    let Object::Dict(d) = resolve_obj(info, data, xref) else {
        return None;
    };
    let registry = pdf_string_or_name(d.get("Registry")?, data, xref)?;
    let ordering = pdf_string_or_name(d.get("Ordering")?, data, xref)?;
    Some((registry, ordering))
}

fn pdf_string_or_name(obj: &Object, data: &[u8], xref: Option<&XRef>) -> Option<String> {
    match resolve_obj(obj, data, xref) {
        Object::Name(n) => Some(n),
        Object::Str(s) => Some(String::from_utf8_lossy(&s).into_owned()),
        _ => None,
    }
}

/// ToUnicode を charcode → String に展開
fn load_tounicode(
    dict: &Dict,
    base_dict: Option<&Dict>,
    data: &[u8],
    xref: Option<&XRef>,
) -> (HashMap<u32, String>, bool) {
    let tu = dict
        .get("ToUnicode")
        .or_else(|| base_dict.and_then(|b| b.get("ToUnicode")));
    let Some(tu) = tu else {
        return (HashMap::new(), false);
    };

    match resolve_obj(tu, data, xref) {
        Object::Name(n) => {
            if n == "Identity-H" || n == "Identity-V" || n == "Identity" {
                return (HashMap::new(), true);
            }
            // その他の名前付き ToUnicode は第2段階
            eprintln!("reader: named ToUnicode '{n}' not loaded yet");
            (HashMap::new(), false)
        }
        Object::Stream(s) => {
            let decoded = match decode_stream_bytes(data, &s, xref) {
                Some(d) => d,
                None => return (HashMap::new(), false),
            };
            let cmap = CMap::parse_embedded(&decoded);
            if cmap.is_identity()
                || cmap.name() == "Identity-H"
                || cmap.name() == "Identity-V"
            {
                return (HashMap::new(), true);
            }
            let mut map = HashMap::new();
            // for_each が Identity 全空間を回すのは避け、疎マップのみ
            // parse 後 identity でなければ map を直接使う
            build_tounicode_from_cmap(&cmap, &mut map);
            (map, false)
        }
        _ => (HashMap::new(), false),
    }
}

fn build_tounicode_from_cmap(cmap: &CMap, map: &mut HashMap<u32, String>) {
    // Identity 全展開はしない
    if cmap.is_identity() {
        return;
    }
    cmap.for_each(|charcode, value| {
        match value {
            MapValue::Cid(cid) => {
                map.insert(charcode, codepoint_to_string(*cid));
            }
            MapValue::Bytes(b) => {
                map.insert(charcode, utf16be_bytes_to_string(b));
            }
        }
    });
}

fn resolve_ascent_descent(
    descriptor: &Dict,
    base_font: Option<&str>,
    data: &[u8],
    xref: Option<&XRef>,
) -> (f64, f64) {
    let mut ascent = descriptor
        .get("Ascent")
        .and_then(|o| as_number(&resolve_obj(o, data, xref)))
        .map(|n| n / 1000.0);
    let mut descent = descriptor
        .get("Descent")
        .and_then(|o| as_number(&resolve_obj(o, data, xref)))
        .map(|n| n / 1000.0);

    // 埋め込み TrueType (FontFile2) の head/hhea で上書き
    if let Some((a, d)) = try_truetype_metrics(descriptor, data, xref) {
        ascent = Some(a);
        descent = Some(d);
    }

    // まだ無い場合は標準14基本メトリクス（非埋め込み向け）
    let need_a = ascent.map(|v| !v.is_finite() || v <= 0.0).unwrap_or(true);
    let need_d = descent.map(|v| !v.is_finite() || v >= 0.0).unwrap_or(true);
    if (need_a || need_d) && !has_embedded_font(descriptor, data, xref) {
        if let Some(raw) = base_font {
            let metrics_name = normalize_font_name(raw);
            if let Some((ba, bd)) = font_basic_metrics(&metrics_name) {
                if need_a {
                    if let Some(a) = ba {
                        ascent = Some(a / 1000.0);
                    }
                }
                if need_d {
                    if let Some(d) = bd {
                        descent = Some(d / 1000.0);
                    }
                }
            }
        }
    }

    (
        correct_ascent(ascent),
        correct_descent(descent),
    )
}

/// 埋め込みフォントプログラムの有無
fn has_embedded_font(descriptor: &Dict, data: &[u8], xref: Option<&XRef>) -> bool {
    for key in ["FontFile", "FontFile2", "FontFile3"] {
        if let Some(o) = descriptor.get(key) {
            match resolve_obj(o, data, xref) {
                Object::Stream(_) | Object::Dict(_) => return true,
                _ => {}
            }
        }
    }
    false
}

/// 標準14メトリクスを引く正規化名（無い場合は serif 判定でフォールバック）
fn resolve_metrics_font_name(base_font: &str) -> String {
    let normalized = normalize_font_name(base_font);
    if has_standard14_metrics(&normalized) {
        return normalized;
    }
    if is_serif_font(base_font) || is_serif_font(&normalized) {
        "Times-Roman".into()
    } else {
        "Helvetica".into()
    }
}

/// Encoding + Differences を解決して defaultEncoding[256] と differences を返す
fn resolve_simple_encoding(
    dict: &Dict,
    subtype: &str,
    flags: i64,
    base_font: &str,
    embedded: bool,
    data: &[u8],
    xref: Option<&XRef>,
) -> ([String; 256], HashMap<u32, String>, Option<String>) {
    let mut differences: HashMap<u32, String> = HashMap::new();
    let mut base_encoding_name: Option<String> = None;

    if let Some(enc_obj) = dict.get("Encoding") {
        match resolve_obj(enc_obj, data, xref) {
            Object::Name(n) => {
                // 名前付きは MacRoman / MacExpert / WinAnsi のみ有効（救済 #8）
                if matches!(
                    n.as_str(),
                    "MacRomanEncoding" | "MacExpertEncoding" | "WinAnsiEncoding"
                ) {
                    base_encoding_name = Some(n);
                }
            }
            Object::Dict(enc_dict) => {
                if let Some(be) = enc_dict.get("BaseEncoding") {
                    if let Object::Name(n) = resolve_obj(be, data, xref) {
                        if matches!(
                            n.as_str(),
                            "MacRomanEncoding"
                                | "MacExpertEncoding"
                                | "WinAnsiEncoding"
                                | "StandardEncoding"
                                | "ExpertEncoding"
                                | "SymbolSetEncoding"
                                | "ZapfDingbatsEncoding"
                        ) {
                            base_encoding_name = Some(n);
                        }
                    }
                }
                if let Some(diff) = enc_dict.get("Differences") {
                    if let Object::Array(arr) = resolve_obj(diff, data, xref) {
                        let mut code: i64 = 0;
                        for item in arr {
                            match resolve_obj(&item, data, xref) {
                                Object::Int(n) => code = n,
                                Object::Name(n) => {
                                    if code >= 0 && code <= 255 {
                                        differences.insert(code as u32, n);
                                    }
                                    code += 1;
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // 非埋め込みシンボルフォントの誤 named encoding を無視（救済 #9）
    let norm = normalize_font_name(base_font);
    if base_encoding_name.is_some() && !embedded && is_symbols_font(&norm) {
        base_encoding_name = None;
    }

    let mut is_symbolic = (flags & FLAG_SYMBOLIC) != 0;
    let is_nonsymbolic = (flags & FLAG_NONSYMBOLIC) != 0;
    // TrueType で両方立ち Differences あり → Symbolic を落とす
    if subtype == "TrueType" && is_symbolic && is_nonsymbolic && !differences.is_empty() {
        is_symbolic = false;
    }

    let table: &[&str; 256] = if let Some(ref n) = base_encoding_name {
        encoding_by_name(n).unwrap_or(&STANDARD)
    } else {
        // 既定: Nonsymbolic → Standard、TrueType で非 Nonsymbolic → WinAnsi
        let mut enc: &[&str; 256] = &STANDARD;
        if subtype == "TrueType" && !is_nonsymbolic {
            enc = &WIN_ANSI;
        }
        if is_symbolic || is_symbols_font(&norm) {
            enc = &MAC_ROMAN;
            if !embedded {
                if norm.contains("Symbol") || base_font.contains("Symbol") {
                    enc = &SYMBOL;
                } else if norm.contains("Dingbats") || base_font.contains("Dingbats") {
                    enc = &ZAPF_DINGBATS;
                } else if base_font.contains("Wingdings") {
                    enc = &WIN_ANSI;
                }
            }
        }
        enc
    };

    let default_encoding: [String; 256] = std::array::from_fn(|i| table[i].to_string());
    (default_encoding, differences, base_encoding_name)
}

/// buildCharCodeToWidth 相当（JS と同様、幅 0 は未登録扱い）
fn build_char_code_to_width(
    metrics_font: &str,
    default_encoding: &[String; 256],
    differences: &HashMap<u32, String>,
) -> HashMap<u32, f64> {
    // Courier 系は等幅で widths 表が空、defaultWidth=600 のみ使う
    if matches!(
        metrics_font,
        "Courier" | "Courier-Bold" | "Courier-BoldOblique" | "Courier-Oblique"
    ) {
        return HashMap::new();
    }
    let mut widths = HashMap::new();
    for char_code in 0u32..256 {
        if let Some(gname) = differences.get(&char_code) {
            if let Some(w) = standard14_glyph_width(metrics_font, gname) {
                if w != 0.0 {
                    widths.insert(char_code, w);
                    continue;
                }
            }
        }
        let gname = &default_encoding[char_code as usize];
        if gname.is_empty() {
            continue;
        }
        if let Some(w) = standard14_glyph_width(metrics_font, gname) {
            if w != 0.0 {
                widths.insert(char_code, w);
            }
        }
    }
    widths
}

/// Encoding+Differences+AGL による Unicode 復元
fn simple_font_to_unicode(
    default_encoding: &[String; 256],
    differences: &HashMap<u32, String>,
    base_encoding_name: Option<&str>,
) -> HashMap<u32, String> {
    let mut encoding: [String; 256] = default_encoding.clone();
    for (&cc, gname) in differences {
        if gname == ".notdef" {
            continue;
        }
        if (cc as usize) < 256 {
            encoding[cc as usize] = gname.clone();
        }
    }

    // Cdd の 10 進失敗時に 16 進で全再解析
    if let Some(map) = try_simple_to_unicode(&encoding, base_encoding_name, false) {
        return map;
    }
    try_simple_to_unicode(&encoding, base_encoding_name, true).unwrap_or_default()
}

fn try_simple_to_unicode(
    encoding: &[String; 256],
    base_encoding_name: Option<&str>,
    force_glyphs: bool,
) -> Option<HashMap<u32, String>> {
    let mut to_unicode = HashMap::new();
    for charcode in 0u32..256 {
        let glyph_name = &encoding[charcode as usize];
        if glyph_name.is_empty() {
            continue;
        }
        if let Some(s) = glyph_unicode(glyph_name) {
            to_unicode.insert(charcode, s.to_string());
            continue;
        }
        // ヒューリスティック
        match glyph_name_to_codepoint(glyph_name, force_glyphs) {
            Ok(Some(code)) => {
                if code > 0 && code <= 0x10_ffff {
                    // baseEncoding があり code==charcode なら base のグリフを優先
                    if let Some(ben) = base_encoding_name {
                        if code == charcode {
                            if let Some(table) = encoding_by_name(ben) {
                                let gn = table[charcode as usize];
                                if !gn.is_empty() {
                                    if let Some(s) = glyph_unicode(gn) {
                                        to_unicode.insert(charcode, s.to_string());
                                        continue;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(c) = char::from_u32(code) {
                        to_unicode.insert(charcode, c.to_string());
                    }
                }
            }
            Ok(None) => {
                // 合字など
                if let Some(s) = ligature_fallback(glyph_name) {
                    to_unicode.insert(charcode, s);
                }
            }
            Err(()) => {
                // Cdd の再解析要求
                if !force_glyphs {
                    return None;
                }
            }
        }
    }
    Some(to_unicode)
}

/// グリフ名ヒューリスティック。Err は Cdd の 16 進再解析要求
fn glyph_name_to_codepoint(glyph_name: &str, force_glyphs: bool) -> Result<Option<u32>, ()> {
    let bytes = glyph_name.as_bytes();
    if bytes.is_empty() {
        return Ok(None);
    }
    match bytes[0] {
        b'G' if glyph_name.len() == 3 => {
            if let Ok(code) = u32::from_str_radix(&glyph_name[1..], 16) {
                return Ok(Some(code));
            }
        }
        b'g' if glyph_name.len() == 5 => {
            if let Ok(code) = u32::from_str_radix(&glyph_name[1..], 16) {
                return Ok(Some(code));
            }
        }
        b'C' | b'c' if (3..=4).contains(&glyph_name.len()) => {
            let code_str = &glyph_name[1..];
            if force_glyphs {
                if let Ok(code) = u32::from_str_radix(code_str, 16) {
                    return Ok(Some(code));
                }
            } else if let Ok(code) = code_str.parse::<u32>() {
                return Ok(Some(code));
            } else if u32::from_str_radix(code_str, 16).is_ok() {
                return Err(());
            }
        }
        b'u' => {
            if let Some(code) = unicode_from_uni_name(glyph_name) {
                return Ok(Some(code));
            }
        }
        _ => {}
    }
    Ok(None)
}

/// uniXXXX / uXXXX{XX}（大文字 hex のみ）
fn unicode_from_uni_name(name: &str) -> Option<u32> {
    let len = name.len();
    let hex = if len == 7 && name.starts_with("uni") {
        &name[3..]
    } else if (5..=7).contains(&len) && name.starts_with('u') {
        &name[1..]
    } else {
        return None;
    };
    if hex.chars().any(|c| c.is_ascii_lowercase()) {
        return None;
    }
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

fn ligature_fallback(glyph_name: &str) -> Option<String> {
    match glyph_name {
        "f_h" | "f_t" | "T_h" => Some(glyph_name.replace('_', "")),
        _ => None,
    }
}

fn correct_ascent(a: Option<f64>) -> f64 {
    match a {
        Some(v) if v.is_finite() && v > 0.0 => v,
        _ => DEFAULT_ASCENT,
    }
}

fn correct_descent(d: Option<f64>) -> f64 {
    match d {
        Some(v) if v.is_finite() && v < 0.0 => v,
        _ => DEFAULT_DESCENT,
    }
}

/// 部分列の検索
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// 埋め込み Type1 (FontFile) の平文部から内蔵エンコーディングを読む
///
/// /Encoding 256 array ... dup <code> /<glyph> put ... readonly def の並びを拾う
/// 名前指定（StandardEncoding）や FontFile 不在は None
fn type1_builtin_encoding(
    descriptor: &Dict,
    data: &[u8],
    xref: Option<&XRef>,
) -> Option<HashMap<u32, String>> {
    let ff = descriptor.get("FontFile")?;
    let stream = match resolve_obj(ff, data, xref) {
        Object::Stream(s) => s,
        _ => return None,
    };
    let bytes = decode_stream_bytes(data, &stream, xref)?;
    // eexec 暗号化部より前の平文だけを見る
    let clear_end = find_sub(&bytes, b"eexec").unwrap_or(bytes.len());
    let clear = &bytes[..clear_end];
    let enc_pos = find_sub(clear, b"/Encoding")?;
    let body = &clear[enc_pos + b"/Encoding".len()..];
    if find_sub(&body[..body.len().min(64)], b"StandardEncoding").is_some() {
        return None;
    }
    // エンコーディング配列の終端まで
    let end = find_sub(body, b" def").unwrap_or(body.len());
    let body = &body[..end];

    let mut map = HashMap::new();
    let mut i = 0;
    while i < body.len() {
        let Some(rel) = find_sub(&body[i..], b"dup ") else {
            break;
        };
        let mut p = i + rel + 4;
        i = p;
        while p < body.len() && body[p] == b' ' {
            p += 1;
        }
        let num_start = p;
        while p < body.len() && body[p].is_ascii_digit() {
            p += 1;
        }
        let Some(code) = std::str::from_utf8(&body[num_start..p])
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|c| *c <= 255)
        else {
            continue;
        };
        while p < body.len() && body[p].is_ascii_whitespace() {
            p += 1;
        }
        if body.get(p) != Some(&b'/') {
            continue;
        }
        p += 1;
        let name_start = p;
        while p < body.len()
            && !body[p].is_ascii_whitespace()
            && !matches!(body[p], b'/' | b'(' | b')' | b'[' | b']' | b'<' | b'>' | b'{' | b'}' | b'%')
        {
            p += 1;
        }
        let name = String::from_utf8_lossy(&body[name_start..p]).into_owned();
        while p < body.len() && body[p].is_ascii_whitespace() {
            p += 1;
        }
        if body[p..].starts_with(b"put") && !name.is_empty() {
            map.insert(code, name);
            i = p;
        }
    }
    if map.is_empty() { None } else { Some(map) }
}

/// FontFile2 から head.unitsPerEm と hhea ascent/descent を読む
fn try_truetype_metrics(
    descriptor: &Dict,
    data: &[u8],
    xref: Option<&XRef>,
) -> Option<(f64, f64)> {
    let ff = descriptor.get("FontFile2")?;
    let stream = match resolve_obj(ff, data, xref) {
        Object::Stream(s) => s,
        _ => return None,
    };
    let bytes = decode_stream_bytes(data, &stream, xref)?;
    parse_ttf_ascent_descent(&bytes)
}

/// TrueType テーブルディレクトリから head / hhea を読む
pub(crate) fn parse_ttf_ascent_descent(font: &[u8]) -> Option<(f64, f64)> {
    if font.len() < 12 {
        return None;
    }
    let num_tables = read_u16(font, 4)? as usize;
    let mut head_off: Option<usize> = None;
    let mut head_len: Option<usize> = None;
    let mut hhea_off: Option<usize> = None;
    let mut hhea_len: Option<usize> = None;

    let mut pos = 12;
    for _ in 0..num_tables {
        if pos + 16 > font.len() {
            break;
        }
        let tag = &font[pos..pos + 4];
        let offset = read_u32(font, pos + 8)? as usize;
        let length = read_u32(font, pos + 12)? as usize;
        match tag {
            b"head" => {
                head_off = Some(offset);
                head_len = Some(length);
            }
            b"hhea" => {
                hhea_off = Some(offset);
                hhea_len = Some(length);
            }
            _ => {}
        }
        pos += 16;
    }

    let head_off = head_off?;
    let head_len = head_len?;
    let hhea_off = hhea_off?;
    let hhea_len = hhea_len?;
    if head_len < 20 || hhea_len < 8 {
        return None;
    }
    if head_off + 20 > font.len() || hhea_off + 8 > font.len() {
        return None;
    }

    let units_per_em = read_u16(font, head_off + 18)? as f64;
    if units_per_em <= 0.0 {
        return None;
    }
    let ascent = read_i16(font, hhea_off + 4)? as f64 / units_per_em;
    let descent = read_i16(font, hhea_off + 6)? as f64 / units_per_em;
    Some((ascent, descent))
}

fn font_matrix_a(dict: &Dict, data: &[u8], xref: Option<&XRef>) -> f64 {
    // 既定 Type3 も PDF では [0.001 0 0 0.001 0 0] が一般的だが、
    // 欠落時は identity 相当の 1.0（lookupMatrix の FONT_IDENTITY_MATRIX）
    // pdf.js FONT_IDENTITY_MATRIX = [1,0,0,1,0,0] for missing; Type3 usually has explicit.
    // evaluator: lookupMatrix(dict.getArray("FontMatrix"), FONT_IDENTITY_MATRIX)
    // FONT_IDENTITY_MATRIX = [0.001,0,0,0.001,0,0] in shared/util.js? 
    // Actually in pdf.js: const FONT_IDENTITY_MATRIX = [0.001, 0, 0, 0.001, 0, 0];
    match dict.get("FontMatrix") {
        Some(o) => match resolve_obj(o, data, xref) {
            Object::Array(a) if !a.is_empty() => {
                as_number(&resolve_obj(&a[0], data, xref)).unwrap_or(0.001)
            }
            _ => 0.001,
        },
        None => 0.001,
    }
}

fn decode_stream_bytes(data: &[u8], stream: &Stream, xref: Option<&XRef>) -> Option<Vec<u8>> {
    struct FetchResolver<'a> {
        xref: Option<&'a XRef>,
        data: &'a [u8],
    }
    impl Resolver for FetchResolver<'_> {
        fn resolve(&self, r: Ref) -> Option<Object> {
            self.xref.and_then(|x| x.fetch(r, self.data).ok())
        }
    }
    decode_stream(
        data,
        stream,
        &FetchResolver { xref, data },
        xref.and_then(|x| x.cipher()),
    )
    .ok()
}

fn resolve_obj(obj: &Object, data: &[u8], xref: Option<&XRef>) -> Object {
    match obj {
        Object::Ref(r) => {
            if let Some(x) = xref {
                x.fetch(*r, data).unwrap_or(Object::Null)
            } else {
                Object::Null
            }
        }
        other => other.clone(),
    }
}

fn dict_int(dict: &Dict, key: &str, data: &[u8], xref: Option<&XRef>) -> Option<i64> {
    dict.get(key)
        .and_then(|o| as_int(&resolve_obj(o, data, xref)))
}

fn as_number(obj: &Object) -> Option<f64> {
    match obj {
        Object::Int(n) => Some(*n as f64),
        Object::Real(n) => Some(*n),
        _ => None,
    }
}

fn as_int(obj: &Object) -> Option<i64> {
    match obj {
        Object::Int(n) => Some(*n),
        Object::Real(n) if n.fract() == 0.0 && n.is_finite() => Some(*n as i64),
        _ => None,
    }
}

fn bytes_as_int(bytes: &[u8]) -> u32 {
    let mut a: u32 = 0;
    for &b in bytes {
        a = (a << 8) | u32::from(b);
    }
    a
}

fn codepoint_to_string(cp: u32) -> String {
    char::from_u32(cp)
        .map(|c| c.to_string())
        .unwrap_or_default()
}

fn read_u16(data: &[u8], off: usize) -> Option<u16> {
    if off + 2 > data.len() {
        return None;
    }
    Some(u16::from_be_bytes([data[off], data[off + 1]]))
}

fn read_i16(data: &[u8], off: usize) -> Option<i16> {
    if off + 2 > data.len() {
        return None;
    }
    Some(i16::from_be_bytes([data[off], data[off + 1]]))
}

fn read_u32(data: &[u8], off: usize) -> Option<u32> {
    if off + 4 > data.len() {
        return None;
    }
    Some(u32::from_be_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[off + 3],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::object::Stream;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// 最小の TrueType: head + hhea だけのテーブルディレクトリ
    fn minimal_ttf(units_per_em: u16, ascent: i16, descent: i16) -> Vec<u8> {
        // offset table: version(4) + numTables(2) + searchRange(2) + entrySelector(2) + rangeShift(2)
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // scaler type
        out.extend_from_slice(&2u16.to_be_bytes()); // numTables
        out.extend_from_slice(&32u16.to_be_bytes()); // searchRange
        out.extend_from_slice(&1u16.to_be_bytes()); // entrySelector
        out.extend_from_slice(&0u16.to_be_bytes()); // rangeShift

        // table directory: 2 * 16 = 32 bytes, then tables
        // head starts at 12 + 32 = 44
        // head needs at least 20 bytes (unitsPerEm at +18)
        // hhea after head: 44 + 54 = 98 (head is 54 bytes typically, we use 20+)
        let head_offset = 12 + 32;
        let head_len = 54usize;
        let hhea_offset = head_offset + head_len;
        let hhea_len = 36usize;

        // head entry
        out.extend_from_slice(b"head");
        out.extend_from_slice(&0u32.to_be_bytes()); // checkSum
        out.extend_from_slice(&(head_offset as u32).to_be_bytes());
        out.extend_from_slice(&(head_len as u32).to_be_bytes());
        // hhea entry
        out.extend_from_slice(b"hhea");
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(hhea_offset as u32).to_be_bytes());
        out.extend_from_slice(&(hhea_len as u32).to_be_bytes());

        // head table
        let mut head = vec![0u8; head_len];
        head[18] = (units_per_em >> 8) as u8;
        head[19] = (units_per_em & 0xff) as u8;
        out.extend_from_slice(&head);

        // hhea table
        let mut hhea = vec![0u8; hhea_len];
        let a = ascent.to_be_bytes();
        let d = descent.to_be_bytes();
        hhea[4] = a[0];
        hhea[5] = a[1];
        hhea[6] = d[0];
        hhea[7] = d[1];
        out.extend_from_slice(&hhea);

        out
    }

    #[test]
    fn stub_chars_latin1_and_space() {
        let font = LoadedFont::stub("F1");
        let gs = font.chars_to_glyphs(b"A \xff");
        assert_eq!(gs.len(), 3);
        assert_eq!(gs[0].0, "A");
        assert!(approx(gs[0].1, 500.0));
        assert!(!gs[0].2);
        assert_eq!(gs[1].0, " ");
        assert!(gs[1].2);
        assert_eq!(gs[2].0, "\u{00ff}");
        assert!(!gs[2].2);
    }

    #[test]
    fn stub_metrics() {
        let font = LoadedFont::stub("F1");
        assert!(approx(font.ascent(), 0.88));
        assert!(approx(font.descent(), -0.12));
    }

    #[test]
    fn simple_widths_and_missing() {
        let mut fd = Dict::new();
        fd.set("MissingWidth", Object::Int(250));
        fd.set("Ascent", Object::Int(800));
        fd.set("Descent", Object::Int(-200));

        let mut font_dict = Dict::new();
        font_dict.set("Subtype", Object::Name("Type1".into()));
        font_dict.set("FirstChar", Object::Int(65));
        font_dict.set("LastChar", Object::Int(67));
        font_dict.set(
            "Widths",
            Object::Array(vec![Object::Int(600), Object::Int(700), Object::Int(800)]),
        );
        font_dict.set("FontDescriptor", Object::Dict(fd));

        let font = load_font_dict(&font_dict, "F1", &[], None);
        let gs = font.chars_to_glyphs(b"ABC");
        assert!(approx(gs[0].1, 600.0));
        assert!(approx(gs[1].1, 700.0));
        assert!(approx(gs[2].1, 800.0));
        // 範囲外 → MissingWidth
        let out = font.chars_to_glyphs(&[0x20]);
        assert!(approx(out[0].1, 250.0));
        assert!(approx(font.ascent(), 0.8));
        assert!(approx(font.descent(), -0.2));
    }

    #[test]
    fn widths_ref_elements_and_non_numeric_skip() {
        // Widths に非数値が混ざる → その位置は default（MissingWidth）
        let mut fd = Dict::new();
        fd.set("MissingWidth", Object::Int(100));
        fd.set("Ascent", Object::Int(900));
        fd.set("Descent", Object::Int(-100));

        let mut font_dict = Dict::new();
        font_dict.set("Subtype", Object::Name("TrueType".into()));
        font_dict.set("FirstChar", Object::Int(0));
        font_dict.set(
            "Widths",
            Object::Array(vec![
                Object::Int(500),
                Object::Name("skip".into()),
                Object::Int(600),
            ]),
        );
        font_dict.set("FontDescriptor", Object::Dict(fd));

        let font = load_font_dict(&font_dict, "F1", &[], None);
        let gs = font.chars_to_glyphs(&[0, 1, 2]);
        assert!(approx(gs[0].1, 500.0));
        // index 1 は非数値スキップで widths に無い → MissingWidth
        assert!(approx(gs[1].1, 100.0));
        assert!(approx(gs[2].1, 600.0));
    }

    #[test]
    fn cid_w_two_forms_and_dw() {
        let mut cid = Dict::new();
        cid.set("Subtype", Object::Name("CIDFontType2".into()));
        cid.set("DW", Object::Int(1000));
        // W: 1 [100 200]  10 12 300
        cid.set(
            "W",
            Object::Array(vec![
                Object::Int(1),
                Object::Array(vec![Object::Int(100), Object::Int(200)]),
                Object::Int(10),
                Object::Int(12),
                Object::Int(300),
            ]),
        );

        let mut type0 = Dict::new();
        type0.set("Subtype", Object::Name("Type0".into()));
        type0.set("Encoding", Object::Name("Identity-H".into()));
        type0.set(
            "DescendantFonts",
            Object::Array(vec![Object::Dict(cid)]),
        );

        let font = load_font_dict(&type0, "C0", &[], None);
        // Identity-H: 2-byte codes. CID = charcode
        // charcode 0x0001 → width 100
        let g = font.chars_to_glyphs(&[0x00, 0x01]);
        assert_eq!(g.len(), 1);
        assert!(approx(g[0].1, 100.0));
        let g = font.chars_to_glyphs(&[0x00, 0x02]);
        assert!(approx(g[0].1, 200.0));
        let g = font.chars_to_glyphs(&[0x00, 0x0a]); // 10
        assert!(approx(g[0].1, 300.0));
        let g = font.chars_to_glyphs(&[0x00, 0x0c]); // 12
        assert!(approx(g[0].1, 300.0));
        // DW
        let g = font.chars_to_glyphs(&[0x00, 0xff]);
        assert!(approx(g[0].1, 1000.0));
    }

    #[test]
    fn type3_width_normalization() {
        let mut font_dict = Dict::new();
        font_dict.set("Subtype", Object::Name("Type3".into()));
        font_dict.set("FirstChar", Object::Int(65));
        font_dict.set("LastChar", Object::Int(65));
        // width in font units = 0.6, FontMatrix[0]=1 → 0.6*1*1000 = 600
        font_dict.set("Widths", Object::Array(vec![Object::Real(0.6)]));
        font_dict.set(
            "FontMatrix",
            Object::Array(vec![
                Object::Real(1.0),
                Object::Int(0),
                Object::Int(0),
                Object::Real(1.0),
                Object::Int(0),
                Object::Int(0),
            ]),
        );

        let font = load_font_dict(&font_dict, "T3", &[], None);
        let g = font.chars_to_glyphs(b"A");
        assert!(approx(g[0].1, 600.0), "got {}", g[0].1);

        // FontMatrix[0]=0.001, width=600 → 600*0.001*1000 = 600
        let mut font_dict2 = Dict::new();
        font_dict2.set("Subtype", Object::Name("Type3".into()));
        font_dict2.set("FirstChar", Object::Int(65));
        font_dict2.set("Widths", Object::Array(vec![Object::Int(600)]));
        font_dict2.set(
            "FontMatrix",
            Object::Array(vec![
                Object::Real(0.001),
                Object::Int(0),
                Object::Int(0),
                Object::Real(0.001),
                Object::Int(0),
                Object::Int(0),
            ]),
        );
        let font2 = load_font_dict(&font_dict2, "T3b", &[], None);
        let g2 = font2.chars_to_glyphs(b"A");
        assert!(approx(g2[0].1, 600.0), "got {}", g2[0].1);
    }

    #[test]
    fn tounicode_stream_bfchar() {
        let cmap_data = br#"
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<41> <0042>
endbfchar
endcmap
"#;
        // Stream without filter: raw in buffer
        let mut stream_dict = Dict::new();
        stream_dict.set("Length", Object::Int(cmap_data.len() as i64));
        // We'll put cmap_data at offset 0 of "data"
        let stream = Stream::new(stream_dict, 0, cmap_data.len());

        let mut font_dict = Dict::new();
        font_dict.set("Subtype", Object::Name("Type1".into()));
        font_dict.set("FirstChar", Object::Int(65));
        font_dict.set("Widths", Object::Array(vec![Object::Int(500)]));
        font_dict.set("ToUnicode", Object::Stream(stream));

        let font = load_font_dict(&font_dict, "F1", cmap_data, None);
        let g = font.chars_to_glyphs(b"A");
        assert_eq!(g[0].0, "B");
    }

    #[test]
    fn ascent_descent_from_descriptor_and_correction() {
        // 欠落 → 0.88 / -0.12
        let font_dict = {
            let mut d = Dict::new();
            d.set("Subtype", Object::Name("Type1".into()));
            d
        };
        let font = load_font_dict(&font_dict, "F1", &[], None);
        assert!(approx(font.ascent(), 0.88));
        assert!(approx(font.descent(), -0.12));

        // descent 正値 → -0.12
        let mut fd = Dict::new();
        fd.set("Ascent", Object::Int(900));
        fd.set("Descent", Object::Int(100)); // wrong sign
        let mut d2 = Dict::new();
        d2.set("Subtype", Object::Name("Type1".into()));
        d2.set("FontDescriptor", Object::Dict(fd));
        let font2 = load_font_dict(&d2, "F2", &[], None);
        assert!(approx(font2.ascent(), 0.9));
        assert!(approx(font2.descent(), -0.12));

        // ascent <= 0 → 0.88
        let mut fd3 = Dict::new();
        fd3.set("Ascent", Object::Int(0));
        fd3.set("Descent", Object::Int(-200));
        let mut d3 = Dict::new();
        d3.set("Subtype", Object::Name("Type1".into()));
        d3.set("FontDescriptor", Object::Dict(fd3));
        let font3 = load_font_dict(&d3, "F3", &[], None);
        assert!(approx(font3.ascent(), 0.88));
        assert!(approx(font3.descent(), -0.2));
    }

    #[test]
    fn truetype_hhea_override() {
        // unitsPerEm=1000, ascent=750, descent=-250 → 0.75 / -0.25
        let ttf = minimal_ttf(1000, 750, -250);
        let (a, d) = parse_ttf_ascent_descent(&ttf).expect("parse ttf");
        assert!(approx(a, 0.75), "ascent={a}");
        assert!(approx(d, -0.25), "descent={d}");

        // FontDescriptor に誤った値 + FontFile2
        let mut stream_dict = Dict::new();
        stream_dict.set("Length", Object::Int(ttf.len() as i64));
        let stream = Stream::new(stream_dict, 0, ttf.len());

        let mut fd = Dict::new();
        fd.set("Ascent", Object::Int(1000));
        fd.set("Descent", Object::Int(-100));
        fd.set("FontFile2", Object::Stream(stream));

        let mut font_dict = Dict::new();
        font_dict.set("Subtype", Object::Name("TrueType".into()));
        font_dict.set("FontDescriptor", Object::Dict(fd));

        let font = load_font_dict(&font_dict, "TT", &ttf, None);
        assert!(approx(font.ascent(), 0.75), "got {}", font.ascent());
        assert!(approx(font.descent(), -0.25), "got {}", font.descent());
    }

    /// 埋め込み Type1 の内蔵エンコーディング（CM 系の合字・記号）を読む
    #[test]
    fn type1_builtin_encoding_ligatures() {
        let pfa: &[u8] = b"%!PS-AdobeFont-1.0: TestFont\n\
/FontName /TestFont def\n\
/Encoding 256 array\n\
0 1 255 {1 index exch /.notdef put} for\n\
dup 11 /ff put\n\
dup 12 /fi put\n\
dup 39 /quoteright put\n\
readonly def\n\
currentfile eexec\n\
00000000";
        let mut sd = Dict::new();
        sd.set("Length", Object::Int(pfa.len() as i64));
        let stream = Stream::new(sd, 0, pfa.len());
        let mut fd = Dict::new();
        fd.set("FontFile", Object::Stream(stream));
        let mut d = Dict::new();
        d.set("Subtype", Object::Name("Type1".into()));
        d.set("BaseFont", Object::Name("TestFont".into()));
        d.set("FontDescriptor", Object::Dict(fd));

        let font = load_font_dict(&d, "T1", pfa, None);
        let g = font.chars_to_glyphs(&[11, 12, 39]);
        assert_eq!(g[0].0, "\u{fb00}", "ff ligature, got {:?}", g[0].0);
        assert_eq!(g[1].0, "\u{fb01}", "fi ligature, got {:?}", g[1].0);
        assert_eq!(g[2].0, "\u{2019}", "quoteright, got {:?}", g[2].0);
    }

    #[test]
    fn composite_embedded_cmap_encoding() {
        let cmap_data = br#"
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidrange
<41> <41> 200
endcidrange
endcmap
"#;
        let mut stream_dict = Dict::new();
        stream_dict.set("Length", Object::Int(cmap_data.len() as i64));
        let stream = Stream::new(stream_dict, 0, cmap_data.len());

        let mut cid = Dict::new();
        cid.set("DW", Object::Int(500));
        cid.set(
            "W",
            Object::Array(vec![
                Object::Int(200),
                Object::Array(vec![Object::Int(777)]),
            ]),
        );

        // Encoding stream is in separate buffer — put both in one buffer:
        // [0..cmap_len) = cmap, we only need cmap for encoding stream
        let mut type0 = Dict::new();
        type0.set("Subtype", Object::Name("Type0".into()));
        type0.set("Encoding", Object::Stream(stream));
        type0.set(
            "DescendantFonts",
            Object::Array(vec![Object::Dict(cid)]),
        );

        let font = load_font_dict(&type0, "C1", cmap_data, None);
        let g = font.chars_to_glyphs(b"A");
        assert_eq!(g.len(), 1);
        assert!(approx(g[0].1, 777.0), "width={}", g[0].1);
    }

    #[test]
    fn word_spacing_only_single_byte_space() {
        let mut cid = Dict::new();
        cid.set("DW", Object::Int(1000));
        let mut type0 = Dict::new();
        type0.set("Subtype", Object::Name("Type0".into()));
        type0.set("Encoding", Object::Name("Identity-H".into()));
        type0.set(
            "DescendantFonts",
            Object::Array(vec![Object::Dict(cid)]),
        );
        let font = load_font_dict(&type0, "C0", &[], None);
        // 2-byte 0x0020 is NOT word-spacing (length != 1)
        let g = font.chars_to_glyphs(&[0x00, 0x20]);
        assert_eq!(g.len(), 1);
        assert!(!g[0].2);
        // simple font space is
        let simple = LoadedFont::stub("S");
        let g2 = simple.chars_to_glyphs(b" ");
        assert!(g2[0].2);
    }

    #[test]
    fn glyphlist_lookup_and_binary_search() {
        assert_eq!(glyph_unicode("A"), Some("A"));
        assert_eq!(glyph_unicode("alpha"), Some("α"));
        assert_eq!(glyph_unicode("notarealglyph"), None);
        // ソート済み
        let list = super::super::data::GLYPHS_UNICODE;
        for w in list.windows(2) {
            assert!(w[0].0 < w[1].0, "{} >= {}", w[0].0, w[1].0);
        }
    }

    #[test]
    fn winansi_slots() {
        assert_eq!(WIN_ANSI[0x41], "A");
        assert_eq!(WIN_ANSI[0x92], "quoteright");
    }

    #[test]
    fn helvetica_and_courier_widths() {
        assert_eq!(standard14_glyph_width("Helvetica", "A"), Some(667.0));
        assert_eq!(standard14_glyph_width("Courier", "A"), Some(600.0));
        assert_eq!(standard14_glyph_width("Courier-Bold", "Z"), Some(600.0));
        assert_eq!(standard14_default_width("Courier"), Some(600.0));
        assert_eq!(standard14_default_width("Helvetica"), Some(0.0));
    }

    #[test]
    fn font_name_normalization() {
        assert_eq!(normalize_font_name("Arial"), "Helvetica");
        assert_eq!(normalize_font_name("ABCDEF+Times-Roman"), "Times-Roman");
        assert_eq!(normalize_font_name("Arial-Bold"), "Helvetica-Bold");
    }

    #[test]
    fn widths_from_standard14_without_widths_array() {
        let mut font_dict = Dict::new();
        font_dict.set("Subtype", Object::Name("Type1".into()));
        font_dict.set("BaseFont", Object::Name("Helvetica".into()));
        // Flags Nonsymbolic → StandardEncoding
        let mut fd = Dict::new();
        fd.set("Flags", Object::Int(FLAG_NONSYMBOLIC));
        font_dict.set("FontDescriptor", Object::Dict(fd));

        let font = load_font_dict(&font_dict, "F1", &[], None);
        let g = font.chars_to_glyphs(b"A");
        assert!(approx(g[0].1, 667.0), "width={}", g[0].1);
        assert_eq!(g[0].0, "A");
    }

    #[test]
    fn differences_override_width_and_unicode() {
        // WinAnsi base + Differences: 65 /W → 幅は W(944)、Unicode は "W"
        // 続けて 66 /alpha → Unicode α（幅は Helvetica に alpha が無ければ default）
        let mut enc = Dict::new();
        enc.set("BaseEncoding", Object::Name("WinAnsiEncoding".into()));
        enc.set(
            "Differences",
            Object::Array(vec![
                Object::Int(65),
                Object::Name("W".into()),
                Object::Name("alpha".into()),
            ]),
        );

        let mut font_dict = Dict::new();
        font_dict.set("Subtype", Object::Name("Type1".into()));
        font_dict.set("BaseFont", Object::Name("Helvetica".into()));
        font_dict.set("Encoding", Object::Dict(enc));
        let mut fd = Dict::new();
        fd.set("Flags", Object::Int(FLAG_NONSYMBOLIC));
        font_dict.set("FontDescriptor", Object::Dict(fd));

        let font = load_font_dict(&font_dict, "F1", &[], None);
        let g = font.chars_to_glyphs(b"AB");
        assert_eq!(g[0].0, "W");
        assert!(approx(g[0].1, 944.0), "width={}", g[0].1);
        assert_eq!(g[1].0, "α");
    }

    #[test]
    fn times_roman_basic_metrics_ascent_descent() {
        let mut font_dict = Dict::new();
        font_dict.set("Subtype", Object::Name("Type1".into()));
        font_dict.set("BaseFont", Object::Name("Times-Roman".into()));
        // FontDescriptor に Ascent/Descent 無し・非埋め込み
        font_dict.set("FontDescriptor", Object::Dict(Dict::new()));

        let font = load_font_dict(&font_dict, "T", &[], None);
        // Times-Roman: ascent 683, descent -217
        assert!(approx(font.ascent(), 0.683), "ascent={}", font.ascent());
        assert!(approx(font.descent(), -0.217), "descent={}", font.descent());
    }

    /// Adobe-Japan1 の CIDSystemInfo
    fn japan1_cid_system_info() -> Dict {
        let mut info = Dict::new();
        info.set("Registry", Object::Str(b"Adobe".to_vec()));
        info.set("Ordering", Object::Str(b"Japan1".to_vec()));
        info.set("Supplement", Object::Int(0));
        info
    }

    /// 90ms-RKSJ-H 複合フォント（/W 付き）
    fn type0_rksj(encoding: &str, with_w: bool) -> Dict {
        let mut cid = Dict::new();
        cid.set("Subtype", Object::Name("CIDFontType0".into()));
        cid.set("CIDSystemInfo", Object::Dict(japan1_cid_system_info()));
        cid.set("DW", Object::Int(1000));
        if with_w {
            // 0x8140 → CID（全角空白）。CID は実行時に確認
            // 幅は load 後に chars_to_glyphs で引く
            let rksj = CMap::load_predefined("90ms-RKSJ-H").expect("rksj");
            let cid_val = match rksj.lookup(0x8140).as_deref() {
                Some(MapValue::Cid(c)) => *c as i64,
                _ => panic!("expected Cid"),
            };
            cid.set(
                "W",
                Object::Array(vec![
                    Object::Int(cid_val),
                    Object::Array(vec![Object::Int(500)]),
                ]),
            );
        }
        let mut type0 = Dict::new();
        type0.set("Subtype", Object::Name("Type0".into()));
        type0.set("Encoding", Object::Name(encoding.into()));
        type0.set(
            "DescendantFonts",
            Object::Array(vec![Object::Dict(cid)]),
        );
        type0
    }

    #[test]
    fn predefined_rksj_h_unicode_width_and_mixed() {
        let type0 = type0_rksj("90ms-RKSJ-H", true);
        let font = load_font_dict(&type0, "Cjk", &[], None);
        assert!(!font.vertical());

        // 0x8140 → 全角空白 U+3000、幅 500
        let g = font.chars_to_glyphs(&[0x81, 0x40]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].0, "\u{3000}");
        assert!(approx(g[0].1, 500.0), "width={}", g[0].1);

        // 1バイト ASCII "A" + 2バイト 0x8140
        let g2 = font.chars_to_glyphs(&[0x41, 0x81, 0x40]);
        assert_eq!(g2.len(), 2);
        assert!(!g2[0].0.is_empty());
        assert_eq!(g2[1].0, "\u{3000}");
    }

    #[test]
    fn predefined_rksj_v_vertical_same_unicode() {
        let h = load_font_dict(&type0_rksj("90ms-RKSJ-H", false), "H", &[], None);
        let v = load_font_dict(&type0_rksj("90ms-RKSJ-V", false), "V", &[], None);
        assert!(v.vertical());
        assert!(!h.vertical());
        let gh = h.chars_to_glyphs(&[0x81, 0x40]);
        let gv = v.chars_to_glyphs(&[0x81, 0x40]);
        assert_eq!(gh[0].0, "\u{3000}");
        assert_eq!(gv[0].0, gh[0].0);
    }

    #[test]
    fn tounicode_takes_priority_over_ucs2() {
        // ToUnicode: 0x8140 → "X"（UCS2 なら U+3000）
        let cmap_data = br#"
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfchar
<8140> <0058>
endbfchar
endcmap
"#;
        let mut stream_dict = Dict::new();
        stream_dict.set("Length", Object::Int(cmap_data.len() as i64));
        let stream = Stream::new(stream_dict, 0, cmap_data.len());

        let mut type0 = type0_rksj("90ms-RKSJ-H", false);
        type0.set("ToUnicode", Object::Stream(stream));

        let font = load_font_dict(&type0, "Cjk", cmap_data, None);
        let g = font.chars_to_glyphs(&[0x81, 0x40]);
        assert_eq!(g[0].0, "X");
    }

    #[test]
    fn identity_h_japan1_ucs2_without_tounicode() {
        // 0x8140 の CID を Identity の charcode として使う
        let rksj = CMap::load_predefined("90ms-RKSJ-H").expect("rksj");
        let cid = match rksj.lookup(0x8140).as_deref() {
            Some(MapValue::Cid(c)) => *c,
            _ => panic!("expected Cid"),
        };

        let mut cid_dict = Dict::new();
        cid_dict.set("Subtype", Object::Name("CIDFontType2".into()));
        cid_dict.set("CIDSystemInfo", Object::Dict(japan1_cid_system_info()));
        cid_dict.set("DW", Object::Int(1000));
        cid_dict.set(
            "W",
            Object::Array(vec![
                Object::Int(cid as i64),
                Object::Array(vec![Object::Int(600)]),
            ]),
        );

        let mut type0 = Dict::new();
        type0.set("Subtype", Object::Name("Type0".into()));
        type0.set("Encoding", Object::Name("Identity-H".into()));
        type0.set(
            "DescendantFonts",
            Object::Array(vec![Object::Dict(cid_dict)]),
        );

        let font = load_font_dict(&type0, "Id", &[], None);
        assert!(!font.vertical());
        // Identity-H は 2 バイト: CID を big-endian で
        let hi = ((cid >> 8) & 0xff) as u8;
        let lo = (cid & 0xff) as u8;
        let g = font.chars_to_glyphs(&[hi, lo]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].0, "\u{3000}");
        assert!(approx(g[0].1, 600.0), "width={}", g[0].1);
        assert!(g[0].3.is_none());
    }

    fn type0_identity_v(with_w2: bool) -> Dict {
        let mut cid = Dict::new();
        cid.set("Subtype", Object::Name("CIDFontType2".into()));
        cid.set("DW", Object::Int(1000));
        // DW2 既定 [880, -1000] を明示
        cid.set(
            "DW2",
            Object::Array(vec![Object::Int(880), Object::Int(-1000)]),
        );
        if with_w2 {
            // CID 0x41: w1y=-800, v1x=400, v1y=700
            cid.set(
                "W2",
                Object::Array(vec![
                    Object::Int(0x41),
                    Object::Array(vec![
                        Object::Int(-800),
                        Object::Int(400),
                        Object::Int(700),
                    ]),
                ]),
            );
        }
        let mut type0 = Dict::new();
        type0.set("Subtype", Object::Name("Type0".into()));
        type0.set("Encoding", Object::Name("Identity-V".into()));
        type0.set(
            "DescendantFonts",
            Object::Array(vec![Object::Dict(cid)]),
        );
        type0
    }

    #[test]
    fn identity_v_default_vmetric_from_dw2() {
        let font = load_font_dict(&type0_identity_v(false), "V", &[], None);
        assert!(font.vertical());
        // 0x0042 → CID 0x42、W2 無し → 既定 [-1000, 500, 880]
        let g = font.chars_to_glyphs(&[0x00, 0x42]);
        assert_eq!(g.len(), 1);
        let vm = g[0].3.expect("vmetric");
        assert!(approx(vm[0], -1000.0), "w1y={}", vm[0]);
        assert!(approx(vm[1], 500.0), "v1x={}", vm[1]);
        assert!(approx(vm[2], 880.0), "v1y={}", vm[2]);
    }

    #[test]
    fn identity_v_w2_overrides_default() {
        let font = load_font_dict(&type0_identity_v(true), "V", &[], None);
        assert!(font.vertical());
        let g = font.chars_to_glyphs(&[0x00, 0x41]);
        let vm = g[0].3.expect("vmetric");
        assert!(approx(vm[0], -800.0));
        assert!(approx(vm[1], 400.0));
        assert!(approx(vm[2], 700.0));
        // 他 CID は既定
        let g2 = font.chars_to_glyphs(&[0x00, 0x42]);
        let vm2 = g2[0].3.expect("vmetric");
        assert!(approx(vm2[0], -1000.0));
        assert!(approx(vm2[1], 500.0));
        assert!(approx(vm2[2], 880.0));
    }

    /// /DW2 は正確な2数値だけを採用し、それ以外は配列全体を既定値へ戻す
    #[test]
    fn identity_v_dw2_validation_and_fallback() {
        let cases = [
            None,
            Some(vec![Object::Int(700), Object::Null]),
            Some(vec![
                Object::Int(700),
                Object::Int(-900),
                Object::Int(0),
            ]),
        ];
        for dw2 in cases {
            let mut cid = Dict::new();
            if let Some(dw2) = dw2 {
                cid.set("DW2", Object::Array(dw2));
            }
            let (_, default) = extract_cid_vmetrics(&cid, 1000.0, &[], None);
            assert_eq!(default, Some([-1000.0, 500.0, 880.0]));
        }

        let mut cid = Dict::new();
        cid.set(
            "DW2",
            Object::Array(vec![Object::Int(700), Object::Int(-900)]),
        );
        let (_, default) = extract_cid_vmetrics(&cid, 1000.0, &[], None);
        assert_eq!(default, Some([-900.0, 500.0, 700.0]));
    }

    /// /W2 の範囲形式を範囲内の全 CID へ展開する
    #[test]
    fn identity_v_w2_range_form() {
        let mut cid = Dict::new();
        cid.set(
            "W2",
            Object::Array(vec![
                Object::Int(0x41),
                Object::Int(0x43),
                Object::Int(-700),
                Object::Int(300),
                Object::Int(600),
            ]),
        );
        let (metrics, default) = extract_cid_vmetrics(&cid, 1000.0, &[], None);
        for code in 0x41..=0x43 {
            assert_eq!(metrics.get(&code), Some(&[-700.0, 300.0, 600.0]));
        }
        assert!(!metrics.contains_key(&0x40));
        assert!(!metrics.contains_key(&0x44));
        assert_eq!(default, Some([-1000.0, 500.0, 880.0]));
    }

    #[test]
    fn identity_h_has_no_vmetric() {
        let mut cid = Dict::new();
        cid.set("Subtype", Object::Name("CIDFontType2".into()));
        cid.set("DW", Object::Int(1000));
        cid.set(
            "DW2",
            Object::Array(vec![Object::Int(880), Object::Int(-1000)]),
        );
        let mut type0 = Dict::new();
        type0.set("Subtype", Object::Name("Type0".into()));
        type0.set("Encoding", Object::Name("Identity-H".into()));
        type0.set(
            "DescendantFonts",
            Object::Array(vec![Object::Dict(cid)]),
        );
        let font = load_font_dict(&type0, "H", &[], None);
        assert!(!font.vertical());
        let g = font.chars_to_glyphs(&[0x00, 0x41]);
        assert!(g[0].3.is_none());
    }
}
