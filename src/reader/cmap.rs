//! 埋め込み CMap・Identity-H/V・定義済み CMap ロード
//!
//! 準拠: pdf.js `cmap.js` / `binary_cmap.js`、specs/06

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::bcmap::decode_bcmap;
use super::data::bcmaps::bcmap_bytes;
use super::lexer::{Lexer, Token};
use crate::error::{Error, Result};

/// 範囲展開の上限（DoS 対策、pdf.js MAX_MAP_RANGE）
const MAX_MAP_RANGE: u32 = 0x00FF_FFFF;

/// CMap マップの値
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MapValue {
    /// CID
    Cid(u32),
    /// bf マップの生バイト（通常 UTF-16BE）
    Bytes(Vec<u8>),
}

/// 複合フォント用 CMap
#[derive(Debug, Clone)]
pub(crate) struct CMap {
    /// バイト長 1..4 ごとの codespace range（low, high）
    codespace_ranges: [Vec<(u32, u32)>; 4],
    /// 多バイトコードを単一整数キーで引く疎マップ
    map: HashMap<u32, MapValue>,
    /// WMode == 1
    vertical: bool,
    name: String,
    /// Identity-H / Identity-V（lookup(code)=code）
    identity: bool,
    /// usecmap で記録した親名（ロードは第2段階）
    use_cmap: Option<String>,
}

impl CMap {
    /// 空の CMap
    pub fn new() -> Self {
        Self {
            codespace_ranges: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            map: HashMap::new(),
            vertical: false,
            name: String::new(),
            identity: false,
            use_cmap: None,
        }
    }

    /// Identity-H（2バイト固定、lookup(code)=code）
    pub fn identity_h() -> Self {
        let mut c = Self::new();
        c.vertical = false;
        c.name = "Identity-H".into();
        c.identity = true;
        c.add_codespace_range(2, 0, 0xffff);
        c
    }

    /// Identity-V
    pub fn identity_v() -> Self {
        let mut c = Self::new();
        c.vertical = true;
        c.name = "Identity-V".into();
        c.identity = true;
        c.add_codespace_range(2, 0, 0xffff);
        c
    }

    /// 名前から Identity 系を作る。非 Identity は None
    pub fn from_identity_name(name: &str) -> Option<Self> {
        match name {
            "Identity-H" => Some(Self::identity_h()),
            "Identity-V" => Some(Self::identity_v()),
            _ => None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    // 縦書き・連鎖・診断用の窓口（後段で使用）
    #[allow(dead_code)]
    pub fn vertical(&self) -> bool {
        self.vertical
    }

    pub fn set_vertical(&mut self, v: bool) {
        self.vertical = v;
    }

    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    pub fn is_identity(&self) -> bool {
        self.identity
    }

    // 縦書き・連鎖・診断用の窓口（後段で使用）
    #[allow(dead_code)]
    pub fn use_cmap(&self) -> Option<&str> {
        self.use_cmap.as_deref()
    }

    /// codespace range が1つでもあるか
    pub fn has_codespace(&self) -> bool {
        self.codespace_ranges.iter().any(|r| !r.is_empty())
    }

    /// 指定バイト長の codespace range（テスト・デバッグ用）
    // 縦書き・連鎖・診断用の窓口（後段で使用）
    #[allow(dead_code)]
    pub fn codespace_ranges_for(&self, n: usize) -> &[(u32, u32)] {
        if (1..=4).contains(&n) {
            &self.codespace_ranges[n - 1]
        } else {
            &[]
        }
    }

    /// キーがマップに存在するか
    pub fn contains(&self, code: u32) -> bool {
        if self.identity {
            return code <= 0xffff;
        }
        self.map.contains_key(&code)
    }

    /// n バイト codespace range を追加（n は 1..4）
    pub fn add_codespace_range(&mut self, n: usize, low: u32, high: u32) {
        if (1..=4).contains(&n) {
            self.codespace_ranges[n - 1].push((low, high));
        }
    }

    /// 親 CMap を継承（usecmap 連鎖）
    pub fn extend_from_parent(&mut self, parent: &CMap) {
        if !self.has_codespace() {
            for i in 0..4 {
                self.codespace_ranges[i] = parent.codespace_ranges[i].clone();
            }
        }
        parent.for_each(|key, value| {
            if !self.contains(key) {
                self.map_one(key, value.clone());
            }
        });
    }

    /// 定義済み CMap を名前からロード
    pub fn load_predefined(name: &str) -> Result<Self> {
        let arc = Self::load_predefined_shared(name, None)?;
        Ok(Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone()))
    }

    /// 定義済み CMap を名前単位で共有ロード
    ///
    /// `cache` があるときは構築結果をメモ化する。ロックは構築中に保持しない
    pub fn load_predefined_shared(
        name: &str,
        cache: Option<&Mutex<HashMap<String, Arc<CMap>>>>,
    ) -> Result<Arc<CMap>> {
        if let Some(cache) = cache {
            if let Ok(guard) = cache.lock() {
                if let Some(c) = guard.get(name) {
                    return Ok(Arc::clone(c));
                }
            }
        }

        let built = Self::build_predefined(name, cache)?;
        let arc = Arc::new(built);

        if let Some(cache) = cache {
            if let Ok(mut guard) = cache.lock() {
                if let Some(c) = guard.get(name) {
                    return Ok(Arc::clone(c));
                }
                guard.insert(name.to_string(), Arc::clone(&arc));
            }
        }
        Ok(arc)
    }

    /// 定義済み CMap を構築する。親は cache 経由でロードする
    fn build_predefined(
        name: &str,
        cache: Option<&Mutex<HashMap<String, Arc<CMap>>>>,
    ) -> Result<Self> {
        if let Some(c) = Self::from_identity_name(name) {
            return Ok(c);
        }
        if !is_built_in_cmap(name) {
            return Err(Error::Reader(format!("Unknown CMap name: {name}")));
        }
        let Some(bytes) = bcmap_bytes(name) else {
            return Err(Error::Reader(format!(
                "CMap not bundled (feature disabled or missing): {name}"
            )));
        };
        let mut cmap = Self::new();
        cmap.set_name(name);
        let use_parent = decode_bcmap(bytes, &mut cmap).map_err(|e| {
            Error::Reader(format!("failed to decode bcmap {name}: {e}"))
        })?;
        if let Some(parent_name) = use_parent {
            cmap.use_cmap = Some(parent_name.clone());
            let parent = Self::load_predefined_shared(&parent_name, cache)?;
            cmap.extend_from_parent(&parent);
        }
        Ok(cmap)
    }

    /// 疎マップへの1エントリ
    pub fn map_one(&mut self, src: u32, dst: MapValue) {
        self.map.insert(src, dst);
    }

    /// CID 範囲展開
    pub fn map_cid_range(&mut self, low: u32, high: u32, mut dst_low: u32) {
        if high < low || high - low > MAX_MAP_RANGE {
            return;
        }
        let mut code = low;
        while code <= high {
            self.map.insert(code, MapValue::Cid(dst_low));
            if code == high {
                break;
            }
            code += 1;
            dst_low = dst_low.saturating_add(1);
        }
    }

    /// bf 範囲展開（dst 末尾バイト繰り上げ）
    pub fn map_bf_range(&mut self, low: u32, high: u32, mut dst: Vec<u8>) {
        if high < low || high - low > MAX_MAP_RANGE || dst.is_empty() {
            return;
        }
        let mut code = low;
        while code <= high {
            self.map.insert(code, MapValue::Bytes(dst.clone()));
            if code == high {
                break;
            }
            code += 1;
            increment_last_byte(&mut dst);
        }
    }

    /// bf 範囲を配列 dst で個別指定
    pub fn map_bf_range_to_array(&mut self, low: u32, high: u32, array: &[Vec<u8>]) {
        if high < low || high - low > MAX_MAP_RANGE {
            return;
        }
        let mut code = low;
        let mut i = 0;
        while code <= high && i < array.len() {
            self.map.insert(code, MapValue::Bytes(array[i].clone()));
            if code == high {
                break;
            }
            code += 1;
            i += 1;
        }
    }

    /// 疎マップ lookup（Bytes は借用、Identity の Cid のみ所有）
    pub fn lookup(&self, code: u32) -> Option<Cow<'_, MapValue>> {
        if self.identity {
            if code <= 0xffff {
                return Some(Cow::Owned(MapValue::Cid(code)));
            }
            return None;
        }
        self.map.get(&code).map(Cow::Borrowed)
    }

    /// マップ全体のイテレート
    pub fn for_each<F: FnMut(u32, &MapValue)>(&self, mut f: F) {
        if self.identity {
            for i in 0u32..=0xffff {
                f(i, &MapValue::Cid(i));
            }
            return;
        }
        for (&k, v) in &self.map {
            f(k, v);
        }
    }

    /// バイト列から (charcode, 消費バイト長) を返す
    ///
    /// どの range にも入らなければ (0, 1)
    pub fn read_char_code(&self, bytes: &[u8], offset: usize) -> (u32, usize) {
        if offset >= bytes.len() {
            return (0, 1);
        }
        let mut c: u32 = 0;
        for n in 0..4 {
            if offset + n >= bytes.len() {
                break;
            }
            c = (c << 8) | u32::from(bytes[offset + n]);
            for &(low, high) in &self.codespace_ranges[n] {
                if low <= c && c <= high {
                    return (c, n + 1);
                }
            }
        }
        (0, 1)
    }

    /// 埋め込みテキスト形式 CMap をパース
    pub fn parse_embedded(data: &[u8]) -> Self {
        let mut cmap = Self::new();
        let mut lexer = Lexer::new(data);
        let mut previous: Option<Token> = None;

        loop {
            let tok = lexer.next_token();
            match &tok {
                Token::Eof => break,
                Token::Name(n) => {
                    if n == "WMode" {
                        if let Token::Int(v) = lexer.next_token() {
                            cmap.vertical = v != 0;
                        }
                    } else if n == "CMapName" {
                        if let Token::Name(nm) = lexer.next_token() {
                            cmap.name = nm;
                        }
                    }
                    previous = Some(tok);
                }
                Token::Cmd(cmd) => match cmd.as_ref() {
                    "endcmap" => break,
                    "usecmap" => {
                        if let Some(Token::Name(nm)) = &previous {
                            cmap.use_cmap = Some(nm.clone());
                        }
                        previous = None;
                    }
                    "begincodespacerange" => {
                        parse_codespace_range(&mut cmap, &mut lexer);
                        previous = None;
                    }
                    "beginbfchar" => {
                        parse_bf_char(&mut cmap, &mut lexer);
                        previous = None;
                    }
                    "begincidchar" => {
                        parse_cid_char(&mut cmap, &mut lexer);
                        previous = None;
                    }
                    "beginbfrange" => {
                        parse_bf_range(&mut cmap, &mut lexer);
                        previous = None;
                    }
                    "begincidrange" => {
                        parse_cid_range(&mut cmap, &mut lexer);
                        previous = None;
                    }
                    _ => {
                        previous = Some(tok);
                    }
                },
                _ => {
                    previous = Some(tok);
                }
            }
        }

        // usecmap 親の継承（失敗時は現状のまま）
        if let Some(parent_name) = cmap.use_cmap.clone() {
            if let Ok(parent) = Self::load_predefined(&parent_name) {
                cmap.extend_from_parent(&parent);
            }
        }

        // 中身が実質 Identity なら差し替え（任意最適化）
        if (cmap.name == "Identity-H" || cmap.name == "Identity-V") && cmap.map.is_empty() {
            let vertical = cmap.name == "Identity-V";
            return if vertical {
                Self::identity_v()
            } else {
                Self::identity_h()
            };
        }

        cmap
    }
}

impl Default for CMap {
    fn default() -> Self {
        Self::new()
    }
}

/// dst 末尾バイトを +1 し、繰り上がりを処理する
fn increment_last_byte(dst: &mut Vec<u8>) {
    if dst.is_empty() {
        return;
    }
    let last = dst.len() - 1;
    if dst[last] < 0xff {
        dst[last] += 1;
        return;
    }
    // 末尾が 0xff → その前のバイトを繰り上げ、末尾を 0
    if last == 0 {
        dst[0] = 0;
        return;
    }
    // pdf.js: substring(0, lastByte-1) + (byte[lastByte-1]+1) + "\x00"
    // ただし lastByte-1 も 0xff の場合はそのまま +1 して溢れる実装
    let prev = last - 1;
    dst[prev] = dst[prev].wrapping_add(1);
    dst[last] = 0;
}

/// 16進文字列等を整数キーに畳む
fn bytes_to_int(bytes: &[u8]) -> u32 {
    let mut a: u32 = 0;
    for &b in bytes {
        a = (a << 8) | u32::from(b);
    }
    a
}

fn expect_str(tok: Token) -> Option<Vec<u8>> {
    match tok {
        Token::Str(s) => Some(s),
        _ => None,
    }
}

fn expect_int(tok: Token) -> Option<i64> {
    match tok {
        Token::Int(n) => Some(n),
        Token::Real(n) if n.fract() == 0.0 && n.is_finite() => Some(n as i64),
        _ => None,
    }
}

fn parse_codespace_range(cmap: &mut CMap, lexer: &mut Lexer<'_>) {
    loop {
        let tok = lexer.next_token();
        match &tok {
            Token::Eof => break,
            Token::Cmd(c) if c == "endcodespacerange" => return,
            Token::Str(low_bytes) => {
                let n = low_bytes.len();
                let low = bytes_to_int(low_bytes);
                let high_tok = lexer.next_token();
                let Some(high_bytes) = expect_str(high_tok) else {
                    // 不正は警告扱いで打ち切り継続
                    return;
                };
                let high = bytes_to_int(&high_bytes);
                let n = if n == 0 { high_bytes.len() } else { n };
                cmap.add_codespace_range(n.max(1).min(4), low, high);
            }
            _ => return,
        }
    }
}

fn parse_bf_char(cmap: &mut CMap, lexer: &mut Lexer<'_>) {
    loop {
        let tok = lexer.next_token();
        match &tok {
            Token::Eof => break,
            Token::Cmd(c) if c == "endbfchar" => return,
            Token::Str(src_bytes) => {
                let src = bytes_to_int(src_bytes);
                let dst_tok = lexer.next_token();
                let Some(dst) = expect_str(dst_tok) else {
                    continue;
                };
                cmap.map_one(src, MapValue::Bytes(dst));
            }
            _ => continue,
        }
    }
}

fn parse_cid_char(cmap: &mut CMap, lexer: &mut Lexer<'_>) {
    loop {
        let tok = lexer.next_token();
        match &tok {
            Token::Eof => break,
            Token::Cmd(c) if c == "endcidchar" => return,
            Token::Str(src_bytes) => {
                let src = bytes_to_int(src_bytes);
                let dst_tok = lexer.next_token();
                let Some(dst) = expect_int(dst_tok) else {
                    continue;
                };
                if dst >= 0 {
                    cmap.map_one(src, MapValue::Cid(dst as u32));
                }
            }
            _ => continue,
        }
    }
}

fn parse_cid_range(cmap: &mut CMap, lexer: &mut Lexer<'_>) {
    loop {
        let tok = lexer.next_token();
        match &tok {
            Token::Eof => break,
            Token::Cmd(c) if c == "endcidrange" => return,
            Token::Str(low_bytes) => {
                let low = bytes_to_int(low_bytes);
                let high_tok = lexer.next_token();
                let Some(high_bytes) = expect_str(high_tok) else {
                    continue;
                };
                let high = bytes_to_int(&high_bytes);
                let dst_tok = lexer.next_token();
                let Some(dst) = expect_int(dst_tok) else {
                    continue;
                };
                if dst >= 0 {
                    cmap.map_cid_range(low, high, dst as u32);
                }
            }
            _ => continue,
        }
    }
}

fn parse_bf_range(cmap: &mut CMap, lexer: &mut Lexer<'_>) {
    loop {
        let tok = lexer.next_token();
        match &tok {
            Token::Eof => break,
            Token::Cmd(c) if c == "endbfrange" => return,
            Token::Str(low_bytes) => {
                let low = bytes_to_int(low_bytes);
                let high_tok = lexer.next_token();
                let Some(high_bytes) = expect_str(high_tok) else {
                    return;
                };
                let high = bytes_to_int(&high_bytes);
                let dst_tok = lexer.next_token();
                match dst_tok {
                    Token::Str(dst) => {
                        cmap.map_bf_range(low, high, dst);
                    }
                    Token::Int(n) if (0..=0xffff).contains(&n) => {
                        // 単一コードポイントを UTF-16BE 2バイトとして扱う
                        let dst = vec![((n as u16) >> 8) as u8, (n as u16) as u8];
                        cmap.map_bf_range(low, high, dst);
                    }
                    Token::ArrayStart => {
                        let mut array = Vec::new();
                        loop {
                            let t = lexer.next_token();
                            match t {
                                Token::ArrayEnd | Token::Eof => break,
                                Token::Str(s) => array.push(s),
                                Token::Int(n) if (0..=0xffff).contains(&n) => {
                                    array.push(vec![((n as u16) >> 8) as u8, (n as u16) as u8]);
                                }
                                _ => {}
                            }
                        }
                        cmap.map_bf_range_to_array(low, high, &array);
                    }
                    _ => return,
                }
            }
            _ => return,
        }
    }
}

/// pdf.js BUILT_IN_CMAPS（163名）
const BUILT_IN_CMAPS: &[&str] = &[
    "Adobe-GB1-UCS2",
    "Adobe-CNS1-UCS2",
    "Adobe-Japan1-UCS2",
    "Adobe-Korea1-UCS2",
    "78-EUC-H",
    "78-EUC-V",
    "78-H",
    "78-RKSJ-H",
    "78-RKSJ-V",
    "78-V",
    "78ms-RKSJ-H",
    "78ms-RKSJ-V",
    "83pv-RKSJ-H",
    "90ms-RKSJ-H",
    "90ms-RKSJ-V",
    "90msp-RKSJ-H",
    "90msp-RKSJ-V",
    "90pv-RKSJ-H",
    "90pv-RKSJ-V",
    "Add-H",
    "Add-RKSJ-H",
    "Add-RKSJ-V",
    "Add-V",
    "Adobe-CNS1-0",
    "Adobe-CNS1-1",
    "Adobe-CNS1-2",
    "Adobe-CNS1-3",
    "Adobe-CNS1-4",
    "Adobe-CNS1-5",
    "Adobe-CNS1-6",
    "Adobe-GB1-0",
    "Adobe-GB1-1",
    "Adobe-GB1-2",
    "Adobe-GB1-3",
    "Adobe-GB1-4",
    "Adobe-GB1-5",
    "Adobe-Japan1-0",
    "Adobe-Japan1-1",
    "Adobe-Japan1-2",
    "Adobe-Japan1-3",
    "Adobe-Japan1-4",
    "Adobe-Japan1-5",
    "Adobe-Japan1-6",
    "Adobe-Korea1-0",
    "Adobe-Korea1-1",
    "Adobe-Korea1-2",
    "B5-H",
    "B5-V",
    "B5pc-H",
    "B5pc-V",
    "CNS-EUC-H",
    "CNS-EUC-V",
    "CNS1-H",
    "CNS1-V",
    "CNS2-H",
    "CNS2-V",
    "ETHK-B5-H",
    "ETHK-B5-V",
    "ETen-B5-H",
    "ETen-B5-V",
    "ETenms-B5-H",
    "ETenms-B5-V",
    "EUC-H",
    "EUC-V",
    "Ext-H",
    "Ext-RKSJ-H",
    "Ext-RKSJ-V",
    "Ext-V",
    "GB-EUC-H",
    "GB-EUC-V",
    "GB-H",
    "GB-V",
    "GBK-EUC-H",
    "GBK-EUC-V",
    "GBK2K-H",
    "GBK2K-V",
    "GBKp-EUC-H",
    "GBKp-EUC-V",
    "GBT-EUC-H",
    "GBT-EUC-V",
    "GBT-H",
    "GBT-V",
    "GBTpc-EUC-H",
    "GBTpc-EUC-V",
    "GBpc-EUC-H",
    "GBpc-EUC-V",
    "H",
    "HKdla-B5-H",
    "HKdla-B5-V",
    "HKdlb-B5-H",
    "HKdlb-B5-V",
    "HKgccs-B5-H",
    "HKgccs-B5-V",
    "HKm314-B5-H",
    "HKm314-B5-V",
    "HKm471-B5-H",
    "HKm471-B5-V",
    "HKscs-B5-H",
    "HKscs-B5-V",
    "Hankaku",
    "Hiragana",
    "KSC-EUC-H",
    "KSC-EUC-V",
    "KSC-H",
    "KSC-Johab-H",
    "KSC-Johab-V",
    "KSC-V",
    "KSCms-UHC-H",
    "KSCms-UHC-HW-H",
    "KSCms-UHC-HW-V",
    "KSCms-UHC-V",
    "KSCpc-EUC-H",
    "KSCpc-EUC-V",
    "Katakana",
    "NWP-H",
    "NWP-V",
    "RKSJ-H",
    "RKSJ-V",
    "Roman",
    "UniCNS-UCS2-H",
    "UniCNS-UCS2-V",
    "UniCNS-UTF16-H",
    "UniCNS-UTF16-V",
    "UniCNS-UTF32-H",
    "UniCNS-UTF32-V",
    "UniCNS-UTF8-H",
    "UniCNS-UTF8-V",
    "UniGB-UCS2-H",
    "UniGB-UCS2-V",
    "UniGB-UTF16-H",
    "UniGB-UTF16-V",
    "UniGB-UTF32-H",
    "UniGB-UTF32-V",
    "UniGB-UTF8-H",
    "UniGB-UTF8-V",
    "UniJIS-UCS2-H",
    "UniJIS-UCS2-HW-H",
    "UniJIS-UCS2-HW-V",
    "UniJIS-UCS2-V",
    "UniJIS-UTF16-H",
    "UniJIS-UTF16-V",
    "UniJIS-UTF32-H",
    "UniJIS-UTF32-V",
    "UniJIS-UTF8-H",
    "UniJIS-UTF8-V",
    "UniJIS2004-UTF16-H",
    "UniJIS2004-UTF16-V",
    "UniJIS2004-UTF32-H",
    "UniJIS2004-UTF32-V",
    "UniJIS2004-UTF8-H",
    "UniJIS2004-UTF8-V",
    "UniJISPro-UCS2-HW-V",
    "UniJISPro-UCS2-V",
    "UniJISPro-UTF8-V",
    "UniJISX0213-UTF32-H",
    "UniJISX0213-UTF32-V",
    "UniJISX02132004-UTF32-H",
    "UniJISX02132004-UTF32-V",
    "UniKS-UCS2-H",
    "UniKS-UCS2-V",
    "UniKS-UTF16-H",
    "UniKS-UTF16-V",
    "UniKS-UTF32-H",
    "UniKS-UTF32-V",
    "UniKS-UTF8-H",
    "UniKS-UTF8-V",
    "V",
    "WP-Symbol",
];

fn is_built_in_cmap(name: &str) -> bool {
    BUILT_IN_CMAPS.contains(&name)
}

/// ToUnicode 用: UTF-16BE バイト列を文字列へ（サロゲート対・奇数長0詰め）
pub(crate) fn utf16be_bytes_to_string(token: &[u8]) -> String {
    let mut bytes = token.to_vec();
    if bytes.len() % 2 != 0 {
        bytes.insert(0, 0);
    }
    let mut out = String::new();
    let mut k = 0;
    while k + 1 < bytes.len() {
        let w1 = (u16::from(bytes[k]) << 8) | u16::from(bytes[k + 1]);
        k += 2;
        // サロゲート領域外
        if (w1 & 0xf800) != 0xd800 {
            if let Some(ch) = char::from_u32(u32::from(w1)) {
                out.push(ch);
            }
            continue;
        }
        // 高サロゲート + 低サロゲート
        if k + 1 >= bytes.len() {
            break;
        }
        let w2 = (u16::from(bytes[k]) << 8) | u16::from(bytes[k + 1]);
        k += 2;
        let cp = (u32::from(w1 & 0x3ff) << 10) + u32::from(w2 & 0x3ff) + 0x1_0000;
        if let Some(ch) = char::from_u32(cp) {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_h_two_byte_read() {
        let cmap = CMap::identity_h();
        // 0x0041 → charcode 0x41, length 2
        let (code, len) = cmap.read_char_code(&[0x00, 0x41, 0x00, 0x42], 0);
        assert_eq!(code, 0x0041);
        assert_eq!(len, 2);
        let (code2, len2) = cmap.read_char_code(&[0x00, 0x41, 0x00, 0x42], 2);
        assert_eq!(code2, 0x0042);
        assert_eq!(len2, 2);
        assert_eq!(cmap.lookup(0x0041).as_deref(), Some(&MapValue::Cid(0x0041)));
    }

    #[test]
    fn read_char_code_no_match_consumes_one() {
        let mut cmap = CMap::new();
        cmap.add_codespace_range(1, 0x20, 0x7e);
        let (code, len) = cmap.read_char_code(&[0x01, 0x41], 0);
        assert_eq!(code, 0);
        assert_eq!(len, 1);
        let (code2, len2) = cmap.read_char_code(b"A", 0);
        assert_eq!(code2, b'A' as u32);
        assert_eq!(len2, 1);
    }

    #[test]
    fn embedded_cidrange_decode() {
        let data = br#"
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> def
/CMapName /Test-H def
/CMapType 1 def
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidrange
<41> <43> 100
endcidrange
endcmap
"#;
        let cmap = CMap::parse_embedded(data);
        assert_eq!(cmap.lookup(0x41).as_deref(), Some(&MapValue::Cid(100)));
        assert_eq!(cmap.lookup(0x42).as_deref(), Some(&MapValue::Cid(101)));
        assert_eq!(cmap.lookup(0x43).as_deref(), Some(&MapValue::Cid(102)));
        let (code, len) = cmap.read_char_code(b"B", 0);
        assert_eq!(code, b'B' as u32);
        assert_eq!(len, 1);
    }

    /// lookup の Bytes 値を UTF-16BE 文字列にするテスト用ヘルパ
    fn bytes_str(cmap: &CMap, code: u32) -> String {
        match cmap.lookup(code).as_deref() {
            Some(MapValue::Bytes(b)) => utf16be_bytes_to_string(b),
            _ => panic!("expected bytes"),
        }
    }

    #[test]
    fn tounicode_bfchar_bfrange_array_surrogate_odd() {
        // bfchar: 0x01 → "A" (0041)
        // bfrange: 0x10..0x11 → starting at 0042 ("B","C")
        // array: 0x20..0x21 → [0044, 0045] ("D","E")
        // surrogate: 0x30 → U+1F600 😀 as D83D DE00
        // odd length: 0x40 → single byte 41 → pad to 0041
        let data = br#"
begincmap
1 begincodespacerange
<00> <FF>
endcodespacerange
1 beginbfchar
<01> <0041>
endbfchar
1 beginbfrange
<10> <11> <0042>
endbfrange
1 beginbfrange
<20> <21> [<0044> <0045>]
endbfrange
1 beginbfchar
<30> <D83DDE00>
endbfchar
1 beginbfchar
<40> <41>
endbfchar
endcmap
"#;
        let cmap = CMap::parse_embedded(data);
        assert_eq!(bytes_str(&cmap, 0x01), "A");
        assert_eq!(bytes_str(&cmap, 0x10), "B");
        assert_eq!(bytes_str(&cmap, 0x11), "C");
        assert_eq!(bytes_str(&cmap, 0x20), "D");
        assert_eq!(bytes_str(&cmap, 0x21), "E");
        assert_eq!(bytes_str(&cmap, 0x30), "\u{1F600}");
        assert_eq!(bytes_str(&cmap, 0x40), "A");
    }

    #[test]
    fn load_90ms_rksj_h_codespace_and_read() {
        let cmap = CMap::load_predefined("90ms-RKSJ-H").expect("load 90ms-RKSJ-H");
        assert!(!cmap.codespace_ranges_for(1).is_empty());
        assert!(!cmap.codespace_ranges_for(2).is_empty());
        let (code, len) = cmap.read_char_code(&[0x41], 0);
        assert_eq!(code, 0x41);
        assert_eq!(len, 1);
        let (code2, len2) = cmap.read_char_code(&[0x81, 0x40], 0);
        assert_eq!(code2, 0x8140);
        assert_eq!(len2, 2);
    }

    #[test]
    fn japan1_cid_to_ucs2_fullwidth_space() {
        let rksj = CMap::load_predefined("90ms-RKSJ-H").expect("load RKSJ");
        let ucs2 = CMap::load_predefined("Adobe-Japan1-UCS2").expect("load UCS2");
        let cid = match rksj.lookup(0x8140).as_deref() {
            Some(MapValue::Cid(c)) => *c,
            other => panic!("expected Cid, got {other:?}"),
        };
        match ucs2.lookup(cid).as_deref() {
            Some(MapValue::Bytes(b)) => {
                let s = utf16be_bytes_to_string(b);
                assert_eq!(s, "\u{3000}");
            }
            other => panic!("expected Bytes, got {other:?}"),
        }
    }

    #[test]
    fn load_90ms_rksj_v_usecmap_and_vertical() {
        let cmap = CMap::load_predefined("90ms-RKSJ-V").expect("load V");
        assert!(cmap.vertical());
        assert_eq!(cmap.use_cmap(), Some("90ms-RKSJ-H"));
        // 親から codespace / マップを継承
        assert!(cmap.has_codespace());
        assert!(cmap.lookup(0x8140).is_some());
    }

    #[test]
    fn embedded_usecmap_inherits_parent() {
        // 親 90ms-RKSJ-H を usecmap し、子は codespace 無しで 0x8140 だけ上書き
        let data = br#"
begincmap
/90ms-RKSJ-H usecmap
1 begincidchar
<8140> 42
endcidchar
endcmap
"#;
        let cmap = CMap::parse_embedded(data);
        assert_eq!(cmap.use_cmap(), Some("90ms-RKSJ-H"));
        // codespace は親から継承
        assert!(cmap.has_codespace());
        assert!(!cmap.codespace_ranges_for(1).is_empty());
        assert!(!cmap.codespace_ranges_for(2).is_empty());
        // 子自身のマッピングが優先
        assert_eq!(cmap.lookup(0x8140).as_deref(), Some(&MapValue::Cid(42)));
        // 親にあって子に無いマッピングは継承される
        let parent = CMap::load_predefined("90ms-RKSJ-H").expect("parent");
        let parent_cid = match parent.lookup(0x8140).as_deref() {
            Some(MapValue::Cid(c)) => *c,
            other => panic!("expected Cid, got {other:?}"),
        };
        assert_ne!(parent_cid, 42, "parent must differ so override is observable");
        // 親の別キー（ASCII）が引き継がれている
        assert_eq!(cmap.lookup(0x41).as_deref(), parent.lookup(0x41).as_deref());
        assert!(cmap.lookup(0x41).is_some());
        // 2 バイト codespace の読み取りも親と同じ
        let (code, len) = cmap.read_char_code(&[0x81, 0x40], 0);
        assert_eq!(code, 0x8140);
        assert_eq!(len, 2);
    }

    #[test]
    fn unknown_cmap_name_errors() {
        let err = CMap::load_predefined("Not-A-Real-CMap").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Unknown CMap name"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn identity_via_load_predefined() {
        let h = CMap::load_predefined("Identity-H").unwrap();
        assert!(h.is_identity());
        assert!(!h.vertical());
        let v = CMap::load_predefined("Identity-V").unwrap();
        assert!(v.is_identity());
        assert!(v.vertical());
    }

    #[test]
    fn feature_gates_bcmap_bytes() {
        use super::super::data::bcmaps::{bcmap_bytes, bundled_features};
        let f = bundled_features();
        // reader 既定では全系統有効
        assert!(f.japan1);
        assert!(f.gb1);
        assert!(bcmap_bytes("90ms-RKSJ-H").is_some());
        assert!(bcmap_bytes("GB-EUC-H").is_some());
        // 未同梱名
        assert!(bcmap_bytes("No-Such-CMap").is_none());
    }
}
