//! 相互参照表（xref）の解析とオブジェクト取得
//!
//! 準拠: pdf.js `xref.js` の readXRef / fetch / fetchCompressed（ストリーミング再開は省略）

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};

use super::cmap::CMap;
use super::crypto::{decrypt_object_tree, CipherFactory};
use super::file::{find_pdf_header_offset, find_startxref};
use super::filters::{decode_stream, is_pdf_whitespace};
use super::font::LoadedFont;
use super::lexer::{Cmd, Lexer, Op, Token};
use super::object::{Dict, Object, Ref, Stream};
use super::parser::{NullResolver, Parser, Resolver};

/// xref エントリ
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum XrefEntry {
    Free,
    Uncompressed { offset: usize, generation: u16 },
    InObjStm { objstm_num: u32, index: u32 },
}

/// 相互参照表とオブジェクトキャッシュ
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct XRef {
    /// `%PDF-` 署名のファイル内オフセット（オフセット基準）
    header_offset: usize,
    entries: HashMap<u32, XrefEntry>,
    /// 最初に得た trailer（/Root /Size /Encrypt /Info の参照元）
    trailer: Dict,
    /// スレッド間で共有するオブジェクトキャッシュ
    cache: Mutex<HashMap<u32, Arc<Object>>>,
    /// 文書内で共有する読み込み済みフォント
    pub(super) font_cache: Arc<Mutex<HashMap<Ref, Arc<LoadedFont>>>>,
    /// 文書内で共有する定義済み CMap（名前 → 構築結果）
    pub(super) cmap_cache: Arc<Mutex<HashMap<String, Arc<CMap>>>>,
    /// 文書内で共有する Form XObject 復号済み本文（参照番号 → 本文）
    pub(super) form_body_cache: Arc<Mutex<HashMap<u32, Arc<Vec<u8>>>>>,
    /// 文書処理中に蓄積した警告
    pub(super) warnings: Arc<Mutex<Vec<String>>>,
    /// スレッド別の取得中参照（循環検出）
    pending: Mutex<HashMap<std::thread::ThreadId, HashSet<Ref>>>,
    /// Standard セキュリティハンドラ（無ければ平文）
    cipher: Option<CipherFactory>,
    /// /Encrypt オブジェクト番号（復号対象外）
    encrypt_num: Option<u32>,
    /// ストリーム復号のチェーン累積出力の上限バイト数
    decode_limit: usize,
    /// 埋め込み CMap の総エントリ数の上限
    cmap_entries_limit: usize,
    /// indexObjects 救済で構築したか（テスト用）
    #[cfg(test)]
    recovered: bool,
}

/// Parser / decode_stream 向けに data を紐づけた Resolver
struct XRefResolver<'a> {
    xref: &'a XRef,
    data: &'a [u8],
}

impl Resolver for XRefResolver<'_> {
    fn resolve(&self, r: Ref) -> Result<Option<Object>> {
        self.xref.fetch(r, self.data).map(Some)
    }
}

#[allow(dead_code)]
impl XRef {
    /// ファイル本体から xref を構築する（パスワードなし）
    pub fn parse(data: &[u8]) -> Result<XRef> {
        Self::parse_with_password(data, None)
    }

    /// パスワード付きで xref を構築し、必要なら暗号ハンドラを初期化する
    ///
    /// 通常解析（startxref → テーブル/ストリーム）が失敗するか、
    /// 解析後に /Root → /Pages が引けない場合は indexObjects へフォールバックする
    pub fn parse_with_password(data: &[u8], password: Option<&str>) -> Result<XRef> {
        Self::parse_with_limit(
            data,
            password,
            crate::extract::DEFAULT_MAX_DECODED_BYTES,
            crate::extract::DEFAULT_MAX_CMAP_ENTRIES,
        )
    }

    /// 復号出力・CMap エントリ数の上限を指定して xref を構築する
    pub fn parse_with_limit(
        data: &[u8],
        password: Option<&str>,
        decode_limit: usize,
        cmap_entries_limit: usize,
    ) -> Result<XRef> {
        match Self::parse_normal(data, password, decode_limit, cmap_entries_limit) {
            Ok(xref) if xref.has_usable_root(data) => Ok(xref),
            // パスワード誤りは構造救済の対象外
            Err(e @ Error::BadPassword) => Err(e),
            _ => Self::index_objects(data, password, decode_limit, cmap_entries_limit),
        }
    }

    /// 空の XRef 骨格を作る
    fn empty(header_offset: usize) -> XRef {
        XRef {
            header_offset,
            entries: HashMap::new(),
            trailer: Dict::new(),
            cache: Mutex::new(HashMap::new()),
            font_cache: Arc::new(Mutex::new(HashMap::new())),
            cmap_cache: Arc::new(Mutex::new(HashMap::new())),
            form_body_cache: Arc::new(Mutex::new(HashMap::new())),
            warnings: Arc::new(Mutex::new(Vec::new())),
            pending: Mutex::new(HashMap::new()),
            cipher: None,
            encrypt_num: None,
            decode_limit: crate::extract::DEFAULT_MAX_DECODED_BYTES,
            cmap_entries_limit: crate::extract::DEFAULT_MAX_CMAP_ENTRIES,
            #[cfg(test)]
            recovered: false,
        }
    }

    /// startxref からの通常解析
    fn parse_normal(
        data: &[u8],
        password: Option<&str>,
        decode_limit: usize,
        cmap_entries_limit: usize,
    ) -> Result<XRef> {
        let header_offset = find_pdf_header_offset(data)?;
        let body = &data[header_offset..];
        let start = find_startxref(body)?;

        let mut xref = Self::empty(header_offset);
        xref.decode_limit = decode_limit;
        xref.cmap_entries_limit = cmap_entries_limit;

        let mut queue: VecDeque<usize> = VecDeque::new();
        queue.push_back(start);
        let mut seen: HashSet<usize> = HashSet::new();
        let mut top_trailer: Option<Dict> = None;

        while let Some(pos) = queue.pop_front() {
            if !seen.insert(pos) {
                continue;
            }
            match xref.read_xref_at(body, pos, &mut queue) {
                Ok(dict) => {
                    if top_trailer.is_none() {
                        top_trailer = Some(dict);
                    }
                }
                Err(_) => {
                    // 壊れた世代は飛ばしてキューを続行する
                }
            }
        }

        xref.trailer = top_trailer.ok_or_else(|| {
            Error::Reader("Failed to parse XRef: no trailer dictionary".into())
        })?;

        xref.init_cipher(data, password)?;
        Ok(xref)
    }

    /// /Encrypt があれば暗号ハンドラを初期化する
    fn init_cipher(&mut self, data: &[u8], password: Option<&str>) -> Result<()> {
        if !self.trailer.has("Encrypt") {
            return Ok(());
        }
        let (encrypt_dict, encrypt_num) = self.resolve_encrypt_dict(data)?;
        self.encrypt_num = encrypt_num;
        let file_id = extract_file_id(&self.trailer);
        self.cipher = Some(CipherFactory::from_encrypt_dict(
            &encrypt_dict,
            &file_id,
            password,
        )?);
        Ok(())
    }

    /// trailer の /Root → /Pages が引けるか
    fn has_usable_root(&self, data: &[u8]) -> bool {
        let Some(root_raw) = self.trailer.get("Root") else {
            return false;
        };
        let fetched;
        let catalog: &Dict = match root_raw {
            Object::Ref(r) => {
                let Ok(o) = self.fetch_shared(*r, data) else {
                    return false;
                };
                fetched = o;
                match &*fetched {
                    Object::Dict(d) => d,
                    _ => return false,
                }
            }
            Object::Dict(d) => d,
            _ => return false,
        };
        let Some(pages_raw) = catalog.get("Pages") else {
            return false;
        };
        match pages_raw {
            Object::Ref(r) => matches!(self.fetch_shared(*r, data).as_deref(), Ok(Object::Dict(_))),
            Object::Dict(_) => true,
            _ => false,
        }
    }

    /// trailer 候補が /Root → /Pages → /Count を満たすか
    fn trailer_pages_valid(&self, data: &[u8], trailer: &Dict) -> bool {
        let Some(root_raw) = trailer.get("Root") else {
            return false;
        };
        let fetched_root;
        let catalog: &Dict = match root_raw {
            Object::Ref(r) => {
                let Ok(o) = self.fetch_shared(*r, data) else {
                    return false;
                };
                fetched_root = o;
                match &*fetched_root {
                    Object::Dict(d) => d,
                    _ => return false,
                }
            }
            Object::Dict(d) => d,
            _ => return false,
        };
        let Some(pages_raw) = catalog.get("Pages") else {
            return false;
        };
        let fetched_pages;
        let pages_dict: &Dict = match pages_raw {
            Object::Ref(r) => {
                let Ok(o) = self.fetch_shared(*r, data) else {
                    return false;
                };
                fetched_pages = o;
                match &*fetched_pages {
                    Object::Dict(d) => d,
                    _ => return false,
                }
            }
            Object::Dict(d) => d,
            _ => return false,
        };
        matches!(pages_dict.get("Count"), Some(Object::Int(_)) | Some(Object::Real(_)))
    }

    /// 壊れた PDF を線形走査して xref を再構築する（pdf.js indexObjects 相当）
    fn index_objects(
        data: &[u8],
        password: Option<&str>,
        decode_limit: usize,
        cmap_entries_limit: usize,
    ) -> Result<XRef> {
        let header_offset = find_pdf_header_offset(data)?;
        let body = &data[header_offset..];

        let mut xref = Self::empty(header_offset);
        xref.decode_limit = decode_limit;
        xref.cmap_entries_limit = cmap_entries_limit;
        #[cfg(test)]
        {
            xref.recovered = true;
        }

        let mut trailers: Vec<usize> = Vec::new();
        let mut xref_stms: Vec<usize> = Vec::new();
        let mut position = 0usize;
        // find_bytes_from の走査は残ファイル全長に比例するため
        // 前方に対象が無いと確認できた後は再探索を打ち切る
        let mut trailer_absent = false;
        let mut startxref_absent = false;

        while position < body.len() {
            let ch = body[position];
            // 空白
            if is_pdf_whitespace(ch) {
                position += 1;
                continue;
            }
            // コメント
            if ch == b'%' {
                position += 1;
                while position < body.len() && body[position] != b'\n' && body[position] != b'\r' {
                    position += 1;
                }
                continue;
            }

            let token = read_scan_token(body, position);

            if token_is_keyword(token, b"xref") {
                // trailer キーワードまで進めて位置を記録
                let tpos = if trailer_absent {
                    None
                } else {
                    find_bytes_from(body, position, b"trailer")
                };
                if let Some(tpos) = tpos {
                    trailers.push(tpos);
                    position = tpos;
                    let sx = if startxref_absent {
                        None
                    } else {
                        find_bytes_from(body, position, b"startxref")
                    };
                    if let Some(sx) = sx {
                        position = sx + b"startxref".len();
                    } else {
                        startxref_absent = true;
                        position = tpos + b"trailer".len();
                    }
                } else {
                    trailer_absent = true;
                    position += token.len().max(1);
                }
            } else if let Some((num, generation, token_len)) = parse_obj_header_token(token) {
                let obj_start = position;
                let content_start = position + token_len;
                let mut update = !xref.entries.contains_key(&num);
                if !update {
                    if let Some(XrefEntry::Uncompressed {
                        generation: eg,
                        ..
                    }) = xref.entries.get(&num)
                    {
                        if *eg == generation {
                            // 同世代の重複はパース可能なら上書き
                            update = can_parse_obj_at(body, content_start);
                        }
                    }
                }
                if update {
                    xref.entries.insert(
                        num,
                        XrefEntry::Uncompressed {
                            offset: obj_start,
                            generation,
                        },
                    );
                }

                // 次の区切りまで進む（endobj 欠落にも耐える）
                let content_end = find_obj_content_end(body, content_start);
                let content = &body[obj_start..content_end.min(body.len())];
                // /XRef タグ（後続が英字でない）を xref ストリーム候補とする
                if let Some(tag_off) = find_bytes_from(content, 0, b"/XRef") {
                    let after = tag_off + 5;
                    let next = content.get(after).copied().unwrap_or(0);
                    if next < 64 {
                        xref_stms.push(obj_start);
                    }
                }
                position = content_end;
            } else if token_is_keyword(token, b"trailer") {
                trailers.push(position);
                let start_pos = position + b"trailer".len();
                // startxref 欠落時は次の obj まで
                if let Some(end) = find_startxref_or_obj(body, start_pos) {
                    position = end;
                } else {
                    position = body.len();
                }
            } else {
                position += token.len().max(1);
            }
        }

        // xref ストリーム候補をマージ（失敗は無視）
        for stm_off in xref_stms {
            let mut queue = VecDeque::new();
            let _ = xref.read_xref_at(body, stm_off, &mut queue);
        }

        // trailer 辞書をパース
        let mut trailer_dicts: Vec<Dict> = Vec::new();
        for &tpos in &trailers {
            if let Some(dict) = parse_trailer_at(body, tpos) {
                trailer_dicts.push(dict);
            }
        }
        // xref ストリーム辞書で /Root を持つものも候補に
        // （read_xref_at が entries だけ更新し trailer は返すが、上では破棄している）
        // 再走査: エントリのうちストリームで /Type /XRef のものを拾う
        let nums: Vec<u32> = xref.entries.keys().copied().collect();
        for num in nums {
            let Ok(obj) = xref.fetch(Ref::new(num, 0), data) else {
                continue;
            };
            let dict = match obj {
                Object::Stream(s) => s.dict,
                Object::Dict(d) => d,
                _ => continue,
            };
            if matches!(dict.get("Type"), Some(Object::Name(n)) if n == "XRef") && dict.has("Root")
            {
                trailer_dicts.push(dict);
            }
            xref.cache.lock().unwrap().clear();
        }

        // trailer 選定: /Root→/Pages→/Count が引けるものを後勝ちで採用
        let mut chosen: Option<Dict> = None;
        let mut fallback: Option<Dict> = None;
        for dict in &trailer_dicts {
            if dict.has("Root") {
                if xref.trailer_pages_valid(data, dict) {
                    chosen = Some(dict.clone());
                } else {
                    fallback = Some(dict.clone());
                }
            }
        }
        if chosen.is_none() {
            chosen = fallback;
        }

        // 全滅時は /Type /Catalog を持つオブジェクトを探す
        if chosen.is_none() || !xref.trailer_pages_valid(data, chosen.as_ref().unwrap_or(&Dict::new()))
        {
            if let Some(catalog_trailer) = xref.find_catalog_trailer(data) {
                chosen = Some(catalog_trailer);
            }
        }

        xref.trailer = chosen.ok_or_else(|| {
            Error::Reader("Invalid PDF structure: no usable trailer in recovery".into())
        })?;

        // キャッシュを空にして暗号初期化（fetch 結果が混ざるのを防ぐ）
        xref.cache.lock().unwrap().clear();
        xref.init_cipher(data, password)?;

        if !xref.has_usable_root(data) {
            return Err(Error::Reader(
                "Invalid PDF structure: recovery could not resolve /Root /Pages".into(),
            ));
        }

        Ok(xref)
    }

    /// 全エントリから /Type /Catalog を探し trailer 相当を組み立てる
    fn find_catalog_trailer(&self, data: &[u8]) -> Option<Dict> {
        let mut nums: Vec<u32> = self.entries.keys().copied().collect();
        nums.sort_unstable();
        for num in nums {
            let generation = match self.entries.get(&num) {
                Some(XrefEntry::Uncompressed { generation, .. }) => *generation,
                _ => 0,
            };
            let obj = match self.fetch(Ref::new(num, generation), data) {
                Ok(o) => o,
                Err(_) => continue,
            };
            let dict = match obj {
                Object::Dict(d) => d,
                Object::Stream(s) => s.dict,
                _ => continue,
            };
            if matches!(dict.get("Type"), Some(Object::Name(n)) if n == "Catalog")
                && dict.has("Pages")
            {
                let mut trailer = Dict::new();
                trailer.set("Root", Object::Ref(Ref::new(num, generation)));
                return Some(trailer);
            }
        }
        None
    }

    /// 救済経路で構築されたか（テスト用）
    #[cfg(test)]
    pub fn is_recovered(&self) -> bool {
        self.recovered
    }

    /// 保持している trailer 辞書
    pub fn trailer(&self) -> &Dict {
        &self.trailer
    }

    /// 暗号ハンドラ（平文なら None）
    pub fn cipher(&self) -> Option<&CipherFactory> {
        self.cipher.as_ref()
    }

    /// ストリーム復号のチェーン累積出力の上限バイト数
    pub(crate) fn decode_limit(&self) -> usize {
        self.decode_limit
    }

    /// 埋め込み CMap の総エントリ数の上限
    pub(crate) fn cmap_entries_limit(&self) -> usize {
        self.cmap_entries_limit
    }

    /// 警告を1件追加する
    pub(crate) fn push_warning(&self, msg: impl Into<String>) {
        if let Ok(mut w) = self.warnings.lock() {
            w.push(msg.into());
        }
    }

    /// 蓄積した警告を取り出し、同じ内容の連続重複を除いて返す
    pub(crate) fn take_warnings(&self) -> Vec<String> {
        let Ok(mut w) = self.warnings.lock() else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::with_capacity(w.len());
        for msg in w.drain(..) {
            if !out.iter().any(|m| m == &msg) {
                out.push(msg);
            }
        }
        out
    }

    /// エントリ表（テスト・診断用）
    #[cfg(test)]
    pub fn entry(&self, num: u32) -> Option<&XrefEntry> {
        self.entries.get(&num)
    }

    /// trailer の /Encrypt を辞書として取る（復号なし）
    fn resolve_encrypt_dict(&self, data: &[u8]) -> Result<(Dict, Option<u32>)> {
        match self.trailer.get("Encrypt") {
            Some(Object::Dict(d)) => Ok((d.clone(), None)),
            Some(Object::Ref(r)) => {
                let obj = self.fetch(*r, data)?;
                match obj {
                    Object::Dict(d) => Ok((d, Some(r.num))),
                    other => Err(Error::Reader(format!(
                        "Encrypt is not a dictionary: {other:?}"
                    ))),
                }
            }
            Some(other) => Err(Error::Reader(format!(
                "invalid /Encrypt value: {other:?}"
            ))),
            None => Err(Error::Reader("missing /Encrypt".into())),
        }
    }

    /// Ref なら fetch、それ以外は clone
    pub fn resolve(&self, obj: &Object, data: &[u8]) -> Object {
        match obj {
            Object::Ref(r) => self.fetch(*r, data).unwrap_or(Object::Null),
            other => other.clone(),
        }
    }

    /// Ref なら fetch_shared、それ以外は clone を包んで返す（読み取り用）
    pub fn resolve_shared(&self, obj: &Object, data: &[u8]) -> Arc<Object> {
        match obj {
            Object::Ref(r) => self
                .fetch_shared(*r, data)
                .unwrap_or_else(|_| Arc::new(Object::Null)),
            other => Arc::new(other.clone()),
        }
    }

    /// 取得中集合へ追加。既に取得中なら false（循環）
    fn pending_insert(&self, r: Ref) -> bool {
        let mut map = self.pending.lock().unwrap();
        map.entry(std::thread::current().id()).or_default().insert(r)
    }

    fn pending_remove(&self, r: Ref) {
        let mut map = self.pending.lock().unwrap();
        if let Some(set) = map.get_mut(&std::thread::current().id()) {
            set.remove(&r);
        }
    }

    /// オブジェクトを取得する（所有コピーが要る呼び出し側向け）
    pub fn fetch(&self, r: Ref, data: &[u8]) -> Result<Object> {
        Ok((*self.fetch_shared(r, data)?).clone())
    }

    /// オブジェクトをキャッシュ共有のまま取得する
    ///
    /// Stream はバッファ上の範囲参照のためキャッシュせずその都度組み立てる
    pub fn fetch_shared(&self, r: Ref, data: &[u8]) -> Result<Arc<Object>> {
        let num = r.num;
        let generation = r.generation;

        if let Some(arc) = self.cache.lock().unwrap().get(&num) {
            return Ok(Arc::clone(arc));
        }

        let entry = match self.entries.get(&num) {
            None | Some(XrefEntry::Free) => {
                return Ok(Arc::new(Object::Null));
            }
            Some(e) => e.clone(),
        };

        if !self.pending_insert(r) {
            return Err(Error::Reader(format!(
                "circular reference detected: {} {}",
                num, generation
            )));
        }

        let result = match &entry {
            XrefEntry::Free => Ok(Object::Null),
            XrefEntry::Uncompressed {
                offset,
                generation: entry_gen,
            } => self.fetch_uncompressed(r, *offset, *entry_gen, data),
            XrefEntry::InObjStm { objstm_num, index } => {
                self.fetch_compressed(r, *objstm_num, *index, data)
            }
        };

        self.pending_remove(r);

        let obj = Arc::new(result?);
        if !matches!(*obj, Object::Stream(_)) {
            self.cache.lock().unwrap().insert(num, Arc::clone(&obj));
        }
        Ok(obj)
    }

    fn fetch_uncompressed(
        &self,
        r: Ref,
        offset: usize,
        entry_gen: u16,
        data: &[u8],
    ) -> Result<Object> {
        if entry_gen != r.generation {
            return Err(Error::Reader(format!(
                "Inconsistent generation in XRef: {} {}",
                r.num, r.generation
            )));
        }

        let abs = self
            .header_offset
            .checked_add(offset)
            .ok_or_else(|| Error::Reader("xref offset overflow".into()))?;
        if abs >= data.len() {
            return Err(Error::Reader(format!(
                "Bad (uncompressed) XRef entry: {} {} (offset out of bounds)",
                r.num, r.generation
            )));
        }

        let resolver = XRefResolver { xref: self, data };
        self.parse_after_obj_header(data, abs, r, &resolver)
    }

    /// オフセット位置の `<num> <gen> obj` を検証し、本体を Parser で読む
    fn parse_after_obj_header(
        &self,
        data: &[u8],
        abs: usize,
        r: Ref,
        resolver: &XRefResolver<'_>,
    ) -> Result<Object> {
        let mut lexer = Lexer::new_at(data, abs);

        let num_tok = lexer.next_token();
        let gen_tok = lexer.next_token();
        let cmd_tok = lexer.next_token();

        let Token::Int(header_num) = num_tok else {
            return Err(Error::Reader(format!(
                "Bad (uncompressed) XRef entry: {} {}",
                r.num, r.generation
            )));
        };
        let Token::Int(header_gen) = gen_tok else {
            return Err(Error::Reader(format!(
                "Bad (uncompressed) XRef entry: {} {}",
                r.num, r.generation
            )));
        };
        if header_num != i64::from(r.num) || header_gen != i64::from(r.generation) {
            return Err(Error::Reader(format!(
                "Bad (uncompressed) XRef entry: {} {}",
                r.num, r.generation
            )));
        }

        match cmd_tok {
            Token::Cmd(Cmd::Op(Op::Obj)) => {
                let mut parser = Parser::new(data, lexer.byte_pos());
                let mut obj = parser.get_obj(resolver)?;
                // /Encrypt オブジェクト自体は復号しない
                let skip = self.encrypt_num == Some(r.num);
                if let Some(cipher) = self.cipher.as_ref() {
                    if !skip {
                        decrypt_object_tree(&mut obj, cipher, r.num, r.generation)?;
                    }
                }
                Ok(obj)
            }
            Token::Cmd(Cmd::Other(cmd)) if cmd.starts_with("obj") => {
                // `obj1234` のようにヘッダが数値と誤結合した救済
                let rest = &cmd[3..];
                if let Ok(n) = rest.parse::<i64>() {
                    return Ok(Object::Int(n));
                }
                Err(Error::Reader(format!(
                    "Bad (uncompressed) XRef entry: {} {}",
                    r.num, r.generation
                )))
            }
            _ => Err(Error::Reader(format!(
                "Bad (uncompressed) XRef entry: {} {}",
                r.num, r.generation
            ))),
        }
    }

    fn fetch_compressed(
        &self,
        r: Ref,
        objstm_num: u32,
        index: u32,
        data: &[u8],
    ) -> Result<Object> {
        let stm_ref = Ref::new(objstm_num, 0);
        let stm_obj = self.fetch_shared(stm_ref, data)?;
        let Object::Stream(stream) = &*stm_obj else {
            return Err(Error::Reader("bad ObjStm stream".into()));
        };

        let first = dict_int(&stream.dict, "First").ok_or_else(|| {
            Error::Reader("invalid first and n parameters for ObjStm stream".into())
        })?;
        let n = dict_int(&stream.dict, "N").ok_or_else(|| {
            Error::Reader("invalid first and n parameters for ObjStm stream".into())
        })?;
        if first < 0 || n < 0 {
            return Err(Error::Reader(
                "invalid first and n parameters for ObjStm stream".into(),
            ));
        }
        let first = first as usize;
        let n = n as usize;

        let resolver = XRefResolver { xref: self, data };
        // ObjStm 本文はストリームとして復号。中身のオブジェクトは再復号しない
        let decoded =
            decode_stream(data, &stream, &resolver, self.cipher.as_ref(), self.decode_limit)?;

        // 先頭の `<num> <相対offset>` ペア N 個
        let mut lexer = Lexer::new_at(&decoded, 0);
        // 正当な入力を弾かない上限: 復号後長を超えるエントリ数はあり得ない
        let cap = n.min(decoded.len());
        let mut nums = Vec::with_capacity(cap);
        let mut offsets = Vec::with_capacity(cap);
        for _ in 0..n {
            let Token::Int(num) = lexer.next_token() else {
                return Err(Error::Reader(
                    "invalid object number in the ObjStm stream".into(),
                ));
            };
            let Token::Int(off) = lexer.next_token() else {
                return Err(Error::Reader(
                    "invalid object offset in the ObjStm stream".into(),
                ));
            };
            if num < 0 || off < 0 {
                return Err(Error::Reader(
                    "invalid object number/offset in the ObjStm stream".into(),
                ));
            }
            nums.push(num as u32);
            offsets.push(off as usize);
        }

        // index と並びの食い違い: まず index、だめなら num 一致で探す
        let mut chosen = if (index as usize) < nums.len() && nums[index as usize] == r.num {
            Some(index as usize)
        } else {
            nums.iter().position(|&x| x == r.num)
        };
        if chosen.is_none() && (index as usize) < nums.len() {
            chosen = Some(index as usize);
        }
        let i = chosen.ok_or_else(|| {
            Error::Reader(format!("Bad (compressed) XRef entry: {} {}", r.num, r.generation))
        })?;

        let obj_start = first
            .checked_add(offsets[i])
            .ok_or_else(|| Error::Reader("Invalid offset in the ObjStm stream.".into()))?;
        if obj_start > decoded.len() {
            return Err(Error::Reader("Invalid offset in the ObjStm stream.".into()));
        }

        let mut parser = Parser::new(&decoded, obj_start);
        let obj = parser.get_obj(&resolver)?;

        // 同ストリーム内の非 Stream オブジェクトをキャッシュ
        if !matches!(obj, Object::Stream(_)) {
            // 呼び出し側 fetch でもキャッシュするが、兄弟も埋める
            for (j, &onum) in nums.iter().enumerate() {
                if j == i {
                    continue;
                }
                if self.cache.lock().unwrap().contains_key(&onum) {
                    continue;
                }
                let start = match first.checked_add(offsets[j]) {
                    Some(s) if s <= decoded.len() => s,
                    _ => continue,
                };
                let mut p = Parser::new(&decoded, start);
                if let Ok(sib) = p.get_obj(&NullResolver) {
                    if !matches!(sib, Object::Stream(_)) {
                        self.cache.lock().unwrap().insert(onum, Arc::new(sib));
                    }
                }
            }
        }

        Ok(obj)
    }

    /// 指定位置の xref テーブルまたは xref ストリームを読む
    fn read_xref_at(
        &mut self,
        body: &[u8],
        pos: usize,
        queue: &mut VecDeque<usize>,
    ) -> Result<Dict> {
        if pos >= body.len() {
            return Err(Error::Reader("Invalid XRef position".into()));
        }

        let mut lexer = Lexer::new_at(body, pos);
        let first = lexer.next_token();

        match first {
            Token::Cmd(Cmd::Op(Op::Xref)) => {
                let dict = self.read_xref_table(body, lexer.byte_pos())?;
                // /XRefStm
                if let Some(Object::Int(off)) = dict.get("XRefStm").cloned() {
                    if off >= 0 {
                        queue.push_back(off as usize);
                    }
                }
                // /Prev
                push_prev(&dict, queue);
                Ok(dict)
            }
            Token::Int(_) => {
                // `<num> <gen> obj` + xref ストリーム
                let gen_tok = lexer.next_token();
                let obj_tok = lexer.next_token();
                let Token::Int(_) = gen_tok else {
                    return Err(Error::Reader("Invalid XRef stream".into()));
                };
                match obj_tok {
                    Token::Cmd(Cmd::Op(Op::Obj)) => {}
                    _ => return Err(Error::Reader("Invalid XRef stream".into())),
                }

                let mut parser = Parser::new(body, lexer.byte_pos());
                // xref ストリーム自身の /Length は通常直接値
                let obj = parser.get_obj(&NullResolver)?;
                let Object::Stream(stream) = obj else {
                    return Err(Error::Reader("Invalid XRef stream".into()));
                };
                let dict = self.read_xref_stream(body, &stream)?;
                push_prev(&dict, queue);
                Ok(dict)
            }
            _ => Err(Error::Reader("Invalid XRef stream header".into())),
        }
    }

    fn read_xref_table(&mut self, body: &[u8], start: usize) -> Result<Dict> {
        let mut lexer = Lexer::new_at(body, start);

        loop {
            let tok = lexer.next_token();
            match tok {
                Token::Cmd(Cmd::Op(Op::Trailer)) => break,
                Token::Int(first_raw) => {
                    let Token::Int(count_raw) = lexer.next_token() else {
                        return Err(Error::Reader(
                            "Invalid XRef table: wrong types in subsection header".into(),
                        ));
                    };
                    if first_raw < 0 || count_raw < 0 {
                        return Err(Error::Reader(
                            "Invalid XRef table: wrong types in subsection header".into(),
                        ));
                    }
                    let mut first = first_raw as u32;
                    let count = count_raw as u32;

                    for i in 0..count {
                        let Token::Int(off) = lexer.next_token() else {
                            return Err(Error::Reader(format!(
                                "Invalid entry in XRef subsection: {first}, {count}"
                            )));
                        };
                        let Token::Int(gen_i) = lexer.next_token() else {
                            return Err(Error::Reader(format!(
                                "Invalid entry in XRef subsection: {first}, {count}"
                            )));
                        };
                        let type_tok = lexer.next_token();
                        let (is_free, is_in_use) = match type_tok {
                            Token::Cmd(Cmd::Op(Op::LowerF)) => (true, false),
                            Token::Cmd(Cmd::Op(Op::LowerN)) => (false, true),
                            _ => {
                                return Err(Error::Reader(format!(
                                    "Invalid entry in XRef subsection: {first}, {count}"
                                )));
                            }
                        };
                        if off < 0 || gen_i < 0 || gen_i > i64::from(u16::MAX) {
                            return Err(Error::Reader(format!(
                                "Invalid entry in XRef subsection: {first}, {count}"
                            )));
                        }

                        // obj0 が free で first==1 のときの補正
                        if i == 0 && is_free && first == 1 {
                            first = 0;
                        }

                        let num = first + i;
                        if self.entries.contains_key(&num) {
                            continue;
                        }
                        let generation = gen_i as u16;
                        let entry = if is_free {
                            XrefEntry::Free
                        } else if is_in_use {
                            XrefEntry::Uncompressed {
                                offset: off as usize,
                                generation,
                            }
                        } else {
                            continue;
                        };
                        self.entries.insert(num, entry);
                    }
                }
                Token::Eof => {
                    return Err(Error::Reader(
                        "Invalid XRef table: could not find trailer dictionary".into(),
                    ));
                }
                _ => {
                    return Err(Error::Reader(
                        "Invalid XRef table: wrong types in subsection header".into(),
                    ));
                }
            }
        }

        // 仕様: 先頭オブジェクトは free
        if let Some(e) = self.entries.get(&0) {
            if !matches!(e, XrefEntry::Free) {
                return Err(Error::Reader(
                    "Invalid XRef table: unexpected first object".into(),
                ));
            }
        }

        let mut parser = Parser::new(body, lexer.byte_pos());
        let trailer_obj = parser.get_obj(&NullResolver)?;
        match trailer_obj {
            Object::Dict(d) => Ok(d),
            Object::Stream(s) => Ok(s.dict),
            _ => Err(Error::Reader(
                "Invalid XRef table: could not parse trailer dictionary".into(),
            )),
        }
    }

    fn read_xref_stream(&mut self, body: &[u8], stream: &Stream) -> Result<Dict> {
        let w = match stream.dict.get("W") {
            Some(Object::Array(a)) if a.len() >= 3 => a,
            _ => {
                return Err(Error::Reader("Invalid XRef entry fields length".into()));
            }
        };
        let type_w = as_nonneg_usize(&w[0]).ok_or_else(|| {
            Error::Reader("Invalid XRef entry fields length".into())
        })?;
        let offset_w = as_nonneg_usize(&w[1]).ok_or_else(|| {
            Error::Reader("Invalid XRef entry fields length".into())
        })?;
        let gen_w = as_nonneg_usize(&w[2]).ok_or_else(|| {
            Error::Reader("Invalid XRef entry fields length".into())
        })?;

        let size = dict_int(&stream.dict, "Size").unwrap_or(0).max(0) as usize;
        let index_pairs: Vec<(u32, u32)> = match stream.dict.get("Index") {
            Some(Object::Array(arr)) => arr
                .chunks_exact(2)
                .map(|pair| {
                    let first = as_nonneg_u32(&pair[0]).ok_or_else(|| {
                        Error::Reader("Invalid XRef range fields".into())
                    })?;
                    let n = as_nonneg_u32(&pair[1]).ok_or_else(|| {
                        Error::Reader("Invalid XRef range fields".into())
                    })?;
                    Ok((first, n))
                })
                .collect::<Result<_>>()?,
            _ => vec![(0, size as u32)],
        };

        // xref ストリームは暗号化対象外
        let decoded = decode_stream(body, stream, &NullResolver, None, self.decode_limit)?;
        let mut cursor = 0usize;
        let entry_width = type_w + offset_w + gen_w;
        // /W [0 0 0] はエントリ境界が取れず /Index 個数だけ空ループするため不正とする
        if entry_width == 0 {
            return Err(Error::Reader("Invalid XRef entry fields length".into()));
        }

        for (first, n) in index_pairs {
            for i in 0..n {
                if entry_width > 0 && cursor + entry_width > decoded.len() {
                    return Err(Error::Reader("Invalid XRef stream data".into()));
                }
                let type_val = if type_w == 0 {
                    1u32
                } else {
                    read_be(&decoded, cursor, type_w)?
                };
                cursor += type_w;
                let field2 = read_be(&decoded, cursor, offset_w)?;
                cursor += offset_w;
                let field3 = read_be(&decoded, cursor, gen_w)?;
                cursor += gen_w;

                let num = first + i;
                if self.entries.contains_key(&num) {
                    continue;
                }
                let entry = match type_val {
                    0 => XrefEntry::Free,
                    1 => XrefEntry::Uncompressed {
                        offset: field2 as usize,
                        generation: field3.min(u32::from(u16::MAX)) as u16,
                    },
                    2 => XrefEntry::InObjStm {
                        objstm_num: field2,
                        index: field3,
                    },
                    other => {
                        return Err(Error::Reader(format!(
                            "Invalid XRef entry type: {other}"
                        )));
                    }
                };
                self.entries.insert(num, entry);
            }
        }

        Ok(stream.dict.clone())
    }
}

impl Resolver for XRef {
    /// キャッシュ済みのみ。未キャッシュは fetch が data を要する
    ///
    /// 通常の Length 解決は `XRefResolver`（fetch 内部）経由。
    /// 外部から `dyn Resolver` として使う場合はキャッシュヒットのみ。
    fn resolve(&self, r: Ref) -> Result<Option<Object>> {
        Ok(self
            .cache
            .lock()
            .unwrap()
            .get(&r.num)
            .map(|arc| (**arc).clone()))
    }
}

/// trailer /ID[0] を取る
fn extract_file_id(trailer: &Dict) -> Vec<u8> {
    match trailer.get("ID") {
        Some(Object::Array(arr)) if !arr.is_empty() => match &arr[0] {
            Object::Str(s) => s.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn push_prev(dict: &Dict, queue: &mut VecDeque<usize>) {
    match dict.get("Prev") {
        Some(Object::Int(n)) if *n >= 0 => {
            queue.push_back(*n as usize);
        }
        Some(Object::Ref(r)) => {
            // 仕様違反の /Prev N 0 R 救済
            queue.push_back(r.num as usize);
        }
        _ => {}
    }
}

fn dict_int(dict: &Dict, key: &str) -> Option<i64> {
    match dict.get(key) {
        Some(Object::Int(n)) => Some(*n),
        Some(Object::Real(n)) => Some(*n as i64),
        _ => None,
    }
}

fn as_nonneg_usize(obj: &Object) -> Option<usize> {
    match obj {
        Object::Int(n) if *n >= 0 => Some(*n as usize),
        Object::Real(n) if *n >= 0.0 => Some(*n as usize),
        _ => None,
    }
}

fn as_nonneg_u32(obj: &Object) -> Option<u32> {
    match obj {
        Object::Int(n) if *n >= 0 && *n <= i64::from(u32::MAX) => Some(*n as u32),
        Object::Real(n) if *n >= 0.0 => Some(*n as u32),
        _ => None,
    }
}

fn read_be(data: &[u8], offset: usize, width: usize) -> Result<u32> {
    if width == 0 {
        return Ok(0);
    }
    if offset + width > data.len() {
        return Err(Error::Reader("Invalid XRef stream data".into()));
    }
    let mut v = 0u32;
    for j in 0..width {
        v = (v << 8) | u32::from(data[offset + j]);
    }
    Ok(v)
}

// --- indexObjects 用スキャナ ---

/// LF / CR / `<` 手前までのトークン（pdf.js readToken 相当）に対する読み出し窓の上限
///
/// 呼び出し先はキーワード（xref/trailer, 先頭 4〜7 バイト）と
/// `<num> <gen> obj` ヘッダ（u32 最大 10 桁 + u16 最大 5 桁 + 空白 + "obj" ≒ 20 バイト）だけを見る
/// 実装契約上、64 バイトあれば正当な識別に足りる
const MAX_SCAN_TOKEN_LEN: usize = 64;

/// LF / CR / `<` 手前までのトークン（pdf.js readToken 相当。無制限だと壊れた入力で O(n²) になるため上限を入れる）
fn read_scan_token(data: &[u8], offset: usize) -> &[u8] {
    let hard_end = offset
        .saturating_add(MAX_SCAN_TOKEN_LEN)
        .min(data.len());
    let mut end = offset;
    while end < hard_end {
        let ch = data[end];
        if ch == b'\n' || ch == b'\r' || ch == b'<' {
            break;
        }
        end += 1;
    }
    &data[offset..end]
}

/// `keyword` で始まり、直後が終端または空白か
fn token_is_keyword(token: &[u8], keyword: &[u8]) -> bool {
    if token.len() < keyword.len() {
        return false;
    }
    if &token[..keyword.len()] != keyword {
        return false;
    }
    if token.len() == keyword.len() {
        return true;
    }
    matches!(
        token[keyword.len()],
        0x00 | 0x09 | 0x0a | 0x0c | 0x0d | 0x20
    )
}

/// `<num> <gen> obj` トークンをパースする
fn parse_obj_header_token(token: &[u8]) -> Option<(u32, u16, usize)> {
    let mut i = 0usize;
    if i >= token.len() || !token[i].is_ascii_digit() {
        return None;
    }
    let num_start = i;
    while i < token.len() && token[i].is_ascii_digit() {
        i += 1;
    }
    let num: u32 = std::str::from_utf8(&token[num_start..i]).ok()?.parse().ok()?;
    if i >= token.len() || !is_pdf_whitespace(token[i]) {
        return None;
    }
    while i < token.len() && is_pdf_whitespace(token[i]) {
        i += 1;
    }
    if i >= token.len() || !token[i].is_ascii_digit() {
        return None;
    }
    let gen_start = i;
    while i < token.len() && token[i].is_ascii_digit() {
        i += 1;
    }
    let generation: u16 = std::str::from_utf8(&token[gen_start..i]).ok()?.parse().ok()?;
    if i >= token.len() || !is_pdf_whitespace(token[i]) {
        return None;
    }
    while i < token.len() && is_pdf_whitespace(token[i]) {
        i += 1;
    }
    let rest = &token[i..];
    if !rest.starts_with(b"obj") {
        return None;
    }
    // "object" など英数字継続は除外
    if rest.len() > 3 && rest[3].is_ascii_alphanumeric() {
        return None;
    }
    Some((num, generation, i + 3))
}

/// オブジェクト本体が NullResolver で読めるか
fn can_parse_obj_at(body: &[u8], content_start: usize) -> bool {
    if content_start >= body.len() {
        return false;
    }
    let mut parser = Parser::new(body, content_start);
    parser.get_obj(&NullResolver).is_ok()
}

/// endobj / 次の obj / xref / trailer のいずれか手前
fn find_obj_content_end(body: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < body.len() {
        // endobj
        if body[i..].starts_with(b"endobj") {
            return i + b"endobj".len();
        }
        // 次の `<num> <gen> obj`（行頭っぽい位置）
        if body[i].is_ascii_digit() {
            let token = read_scan_token(body, i);
            if parse_obj_header_token(token).is_some() {
                return i;
            }
        }
        if body[i..].starts_with(b"xref") {
            let after = i + 4;
            if after >= body.len() || is_pdf_whitespace(body[after]) {
                return i;
            }
        }
        if body[i..].starts_with(b"trailer") {
            let after = i + 7;
            if after >= body.len() || is_pdf_whitespace(body[after]) || body[after] == b'<' {
                return i;
            }
        }
        i += 1;
    }
    body.len()
}

fn find_bytes_from(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// trailer 直後から startxref または次の obj の終端位置
fn find_startxref_or_obj(body: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < body.len() {
        if body[i..].starts_with(b"startxref") {
            return Some(i + b"startxref".len());
        }
        if body[i].is_ascii_digit() {
            let token = read_scan_token(body, i);
            if parse_obj_header_token(token).is_some() {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// `trailer` キーワード位置から辞書を読む
fn parse_trailer_at(body: &[u8], trailer_pos: usize) -> Option<Dict> {
    // "trailer" を読み飛ばして辞書をパース
    let mut pos = trailer_pos;
    if body[pos..].starts_with(b"trailer") {
        pos += b"trailer".len();
    }
    let mut parser = Parser::new(body, pos);
    match parser.get_obj(&NullResolver) {
        Ok(Object::Dict(d)) => Some(d),
        Ok(Object::Stream(s)) => Some(s.dict),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::font::lookup_font;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    /// 最小 PDF（Catalog + Pages）を組み立てる
    fn build_minimal_pdf() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.4\n");

        let o1 = body.len();
        body.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let o2 = body.len();
        body.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

        let xref_off = body.len();
        body.extend_from_slice(b"xref\n0 3\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o1).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o2).as_bytes());
        body.extend_from_slice(b"trailer\n<< /Size 3 /Root 1 0 R >>\n");
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{xref_off}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");
        body
    }

    /// 同じオブジェクト番号に異なるフォントを持つ最小 PDF
    fn build_pdf_with_font(base_font: &str) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.4\n");

        let o1 = body.len();
        body.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let o2 = body.len();
        body.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

        let o3 = body.len();
        body.extend_from_slice(
            format!("3 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /{base_font} >>\nendobj\n")
                .as_bytes(),
        );

        let xref_off = body.len();
        body.extend_from_slice(b"xref\n0 4\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        body.extend_from_slice(format!("{o1:010} 00000 n \n").as_bytes());
        body.extend_from_slice(format!("{o2:010} 00000 n \n").as_bytes());
        body.extend_from_slice(format!("{o3:010} 00000 n \n").as_bytes());
        body.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n");
        body.extend_from_slice(format!("{xref_off}\n%%EOF\n").as_bytes());
        body
    }

    #[test]
    fn font_cache_is_shared_only_within_document() {
        fn resources() -> Dict {
            let mut fonts = Dict::new();
            fonts.set("F1", Object::Ref(Ref::new(3, 0)));
            let mut resources = Dict::new();
            resources.set("Font", Object::Dict(fonts));
            resources
        }

        let data_a = build_pdf_with_font("Helvetica");
        let data_b = build_pdf_with_font("Courier");
        let xref_a = XRef::parse(&data_a).expect("parse first document");
        let xref_b = XRef::parse(&data_b).expect("parse second document");
        assert!(!Arc::ptr_eq(&xref_a.font_cache, &xref_b.font_cache));

        let font_a = lookup_font(&resources(), "F1", &data_a, Some(&xref_a));
        let font_a_cached = lookup_font(&resources(), "F1", &data_a, Some(&xref_a));
        let font_b = lookup_font(&resources(), "F1", &data_b, Some(&xref_b));
        assert!(Arc::ptr_eq(&font_a, &font_a_cached));
        assert!(!Arc::ptr_eq(&font_a, &font_b));
        assert_eq!(font_a.display_name(), "Helvetica");
        assert_eq!(font_b.display_name(), "Courier");
    }

    #[test]
    fn parse_and_fetch_minimal() {
        let data = build_minimal_pdf();
        let xref = XRef::parse(&data).expect("parse");
        let root = xref
            .fetch(Ref::new(1, 0), &data)
            .expect("fetch catalog");
        let Object::Dict(d) = root else {
            panic!("expected Dict, got {root:?}");
        };
        assert!(matches!(d.get("Type"), Some(Object::Name(n)) if n == "Catalog"));
        assert!(matches!(
            d.get("Pages"),
            Some(Object::Ref(Ref {
                num: 2,
                generation: 0
            }))
        ));

        let pages = xref.fetch(Ref::new(2, 0), &data).expect("fetch pages");
        let Object::Dict(p) = pages else {
            panic!("expected Dict");
        };
        assert!(matches!(p.get("Type"), Some(Object::Name(n)) if n == "Pages"));
        assert!(matches!(xref.trailer().get("Root"), Some(Object::Ref(_))));
    }

    #[test]
    fn free_and_missing_are_null() {
        let data = build_minimal_pdf();
        let xref = XRef::parse(&data).expect("parse");
        assert!(matches!(
            xref.fetch(Ref::new(0, 65535), &data).unwrap(),
            Object::Null
        ));
        assert!(matches!(
            xref.fetch(Ref::new(99, 0), &data).unwrap(),
            Object::Null
        ));
    }

    /// 増分更新: 新しい xref が先に処理され、既存エントリは上書きされない
    #[test]
    fn incremental_update_newer_wins() {
        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.4\n");

        // 旧オブジェクト 1
        let o1_old = body.len();
        body.extend_from_slice(b"1 0 obj\n<< /Type /Old /Pages << /Kids [] /Count 0 >> >>\nendobj\n");

        let xref_old = body.len();
        body.extend_from_slice(b"xref\n0 2\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o1_old).as_bytes());
        body.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R >>\n");
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{xref_old}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");

        // 増分: 新しいオブジェクト 1
        let o1_new = body.len();
        body.extend_from_slice(b"1 0 obj\n<< /Type /New /Pages << /Kids [] /Count 0 >> >>\nendobj\n");

        let xref_new = body.len();
        body.extend_from_slice(b"xref\n0 2\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o1_new).as_bytes());
        body.extend_from_slice(
            format!("trailer\n<< /Size 2 /Root 1 0 R /Prev {xref_old} >>\n").as_bytes(),
        );
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{xref_new}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");

        let xref = XRef::parse(&body).expect("parse");
        let obj = xref.fetch(Ref::new(1, 0), &body).expect("fetch");
        let Object::Dict(d) = obj else {
            panic!("expected Dict");
        };
        assert!(
            matches!(d.get("Type"), Some(Object::Name(n)) if n == "New"),
            "newer generation should win, got {d:?}"
        );
    }

    /// xref ストリーム（Flate 圧縮）からエントリを読む
    #[test]
    fn xref_stream_flate() {
        // オブジェクト本体
        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.5\n");

        let o1 = body.len();
        body.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let o2 = body.len();
        body.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");

        // W = [1 2 2]: type(1), offset(2), gen(2)
        // entries: 0 free, 1 -> o1, 2 -> o2
        let mut raw = Vec::new();
        // obj 0: type0 free
        raw.push(0);
        raw.extend_from_slice(&0u16.to_be_bytes());
        raw.extend_from_slice(&65535u16.to_be_bytes());
        // obj 1
        raw.push(1);
        raw.extend_from_slice(&(o1 as u16).to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());
        // obj 2
        raw.push(1);
        raw.extend_from_slice(&(o2 as u16).to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());

        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&raw).unwrap();
        let compressed = enc.finish().unwrap();

        let stm_off = body.len();
        // オブジェクト番号 3 が xref ストリーム
        let dict = format!(
            "3 0 obj\n<< /Type /XRef /Size 3 /W [1 2 2] /Root 1 0 R /Filter /FlateDecode /Length {} >>\nstream\n",
            compressed.len()
        );
        body.extend_from_slice(dict.as_bytes());
        body.extend_from_slice(&compressed);
        body.extend_from_slice(b"\nendstream\nendobj\n");
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{stm_off}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");

        // o1/o2 が 65535 を超えない前提（このテストの PDF は十分小さい）
        assert!(o1 <= u16::MAX as usize && o2 <= u16::MAX as usize);

        let xref = XRef::parse(&body).expect("parse xref stream");
        let cat = xref.fetch(Ref::new(1, 0), &body).expect("catalog");
        assert!(matches!(cat, Object::Dict(ref d) if matches!(d.get("Type"), Some(Object::Name(n)) if n == "Catalog")));
    }

    /// ObjStm 内オブジェクトの取得
    #[test]
    fn fetch_from_objstm() {
        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.5\n");

        // ObjStm (object 5): objects 1 and 2。先頭に num/offset ペア、First 以降が本体
        let o1_bytes = b"<< /Type /Catalog /Pages 2 0 R >>";
        let o2_bytes = b"<< /Type /Pages /Kids [] /Count 0 >>";
        let off2 = o1_bytes.len();
        let pair = format!("1 0 2 {off2} ");
        let first = pair.len();
        let mut stm_data = Vec::new();
        stm_data.extend_from_slice(pair.as_bytes());
        stm_data.extend_from_slice(o1_bytes);
        stm_data.extend_from_slice(o2_bytes);

        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&stm_data).unwrap();
        let compressed = enc.finish().unwrap();

        let objstm_off = body.len();
        let stm_dict = format!(
            "5 0 obj\n<< /Type /ObjStm /N 2 /First {first} /Filter /FlateDecode /Length {} >>\nstream\n",
            compressed.len()
        );
        body.extend_from_slice(stm_dict.as_bytes());
        body.extend_from_slice(&compressed);
        body.extend_from_slice(b"\nendstream\nendobj\n");

        // xref stream: 0 free, 1 in objstm 5 idx 0, 2 in objstm 5 idx 1, 5 uncompressed
        // W = [1 1 1] carefully with small numbers - use [1 2 2]
        let mut raw = Vec::new();
        // 0 free
        raw.push(0);
        raw.extend_from_slice(&0u16.to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());
        // 1 compressed: objstm 5, index 0
        raw.push(2);
        raw.extend_from_slice(&5u16.to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());
        // 2 compressed: objstm 5, index 1
        raw.push(2);
        raw.extend_from_slice(&5u16.to_be_bytes());
        raw.extend_from_slice(&1u16.to_be_bytes());
        // 3 unused - skip by Index
        // We'll use Index [0 3 5 1] for objects 0,1,2 and 5
        // Actually Size=6 with entries for 0,1,2,3,4,5 - mark 3,4 free
        // 3 free
        raw.push(0);
        raw.extend_from_slice(&0u16.to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());
        // 4 free
        raw.push(0);
        raw.extend_from_slice(&0u16.to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());
        // 5 uncompressed objstm
        raw.push(1);
        raw.extend_from_slice(&(objstm_off as u16).to_be_bytes());
        raw.extend_from_slice(&0u16.to_be_bytes());

        assert!(objstm_off <= u16::MAX as usize);

        let mut enc2 = ZlibEncoder::new(Vec::new(), Compression::default());
        enc2.write_all(&raw).unwrap();
        let xref_compressed = enc2.finish().unwrap();

        let xref_off = body.len();
        let xref_dict = format!(
            "6 0 obj\n<< /Type /XRef /Size 6 /W [1 2 2] /Root 1 0 R /Filter /FlateDecode /Length {} >>\nstream\n",
            xref_compressed.len()
        );
        body.extend_from_slice(xref_dict.as_bytes());
        body.extend_from_slice(&xref_compressed);
        body.extend_from_slice(b"\nendstream\nendobj\n");
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{xref_off}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");

        let xref = XRef::parse(&body).expect("parse");
        assert!(matches!(
            xref.entry(1),
            Some(XrefEntry::InObjStm {
                objstm_num: 5,
                index: 0
            })
        ));
        let cat = xref.fetch(Ref::new(1, 0), &body).expect("fetch 1 from objstm");
        let Object::Dict(d) = cat else {
            panic!("expected Dict, got {cat:?}");
        };
        assert!(matches!(d.get("Type"), Some(Object::Name(n)) if n == "Catalog"));

        let pages = xref.fetch(Ref::new(2, 0), &body).expect("fetch 2");
        let Object::Dict(p) = pages else {
            panic!("expected Dict");
        };
        assert!(matches!(p.get("Type"), Some(Object::Name(n)) if n == "Pages"));
    }

    /// /Length が間接参照のストリームを XRef 経由で解決
    #[test]
    fn stream_length_via_xref_resolver() {
        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.4\n");

        // 3 0 obj = Length value 5
        let o3 = body.len();
        body.extend_from_slice(b"3 0 obj\n5\nendobj\n");

        // 1 0 obj = stream with /Length 3 0 R
        let o1 = body.len();
        body.extend_from_slice(b"1 0 obj\n<< /Length 3 0 R >>\nstream\nhello\nendstream\nendobj\n");

        // 2 0 obj = catalog with a minimal /Pages
        let o2 = body.len();
        body.extend_from_slice(
            b"2 0 obj\n<< /Type /Catalog /Pages << /Kids [] /Count 0 >> >>\nendobj\n",
        );

        let xref_off = body.len();
        body.extend_from_slice(b"xref\n0 4\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o1).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o2).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o3).as_bytes());
        body.extend_from_slice(b"trailer\n<< /Size 4 /Root 2 0 R >>\n");
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{xref_off}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");

        let xref = XRef::parse(&body).expect("parse");
        let obj = xref.fetch(Ref::new(1, 0), &body).expect("fetch stream");
        let Object::Stream(s) = obj else {
            panic!("expected Stream, got {obj:?}");
        };
        assert_eq!(s.length, 5);
        assert_eq!(&body[s.start..s.start + s.length], b"hello");
    }

    /// 互いを /Length に指す循環参照がエラーになる
    #[test]
    fn circular_length_refs_error() {
        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.4\n");

        // 1 0 obj stream Length -> 2 0 R
        // content must not contain "endstream" early; use short content
        let o1 = body.len();
        body.extend_from_slice(
            b"1 0 obj\n<< /Length 2 0 R >>\nstream\nXXXX\nendstream\nendobj\n",
        );

        // 2 0 obj stream Length -> 1 0 R
        let o2 = body.len();
        body.extend_from_slice(
            b"2 0 obj\n<< /Length 1 0 R >>\nstream\nYYYY\nendstream\nendobj\n",
        );

        let o3 = body.len();
        body.extend_from_slice(
            b"3 0 obj\n<< /Type /Catalog /Pages << /Kids [] /Count 0 >> >>\nendobj\n",
        );

        let xref_off = body.len();
        body.extend_from_slice(b"xref\n0 4\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o1).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o2).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o3).as_bytes());
        body.extend_from_slice(b"trailer\n<< /Size 4 /Root 3 0 R >>\n");
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{xref_off}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");

        let xref = XRef::parse(&body).expect("parse");
        let err = xref.fetch(Ref::new(1, 0), &body).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.to_lowercase().contains("circular"),
            "expected circular error, got: {msg}"
        );
    }

    #[test]
    fn header_offset_prefix() {
        let mut data = vec![0x00, 0x01, 0x02, b'x'];
        let pdf = build_minimal_pdf();
        data.extend_from_slice(&pdf);
        let xref = XRef::parse(&data).expect("parse with prefix");
        let root = xref.fetch(Ref::new(1, 0), &data).expect("fetch");
        assert!(matches!(root, Object::Dict(_)));
    }

    #[test]
    fn resolve_helper() {
        let data = build_minimal_pdf();
        let xref = XRef::parse(&data).expect("parse");
        let r = Object::Ref(Ref::new(1, 0));
        let resolved = xref.resolve(&r, &data);
        assert!(matches!(resolved, Object::Dict(_)));
        let n = Object::Int(42);
        assert!(matches!(xref.resolve(&n, &data), Object::Int(42)));
    }

    /// 1ページ付き最小 PDF（Pages /Count 1）
    fn build_one_page_pdf() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(b"%PDF-1.4\n");

        let o1 = body.len();
        body.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        let o2 = body.len();
        body.extend_from_slice(
            b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );

        let o3 = body.len();
        body.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> >>\nendobj\n",
        );

        let xref_off = body.len();
        body.extend_from_slice(b"xref\n0 4\n");
        body.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o1).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o2).as_bytes());
        body.extend_from_slice(format!("{:010} 00000 n \n", o3).as_bytes());
        body.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
        body.extend_from_slice(b"startxref\n");
        body.extend_from_slice(format!("{xref_off}\n").as_bytes());
        body.extend_from_slice(b"%%EOF\n");
        body
    }

    /// startxref オフセットが誤った値でも recovery で開ける
    #[test]
    fn recovery_broken_startxref_offset() {
        let mut data = build_one_page_pdf();
        // startxref 直後の数値を 0 に差し替え
        let sx = data
            .windows(9)
            .rposition(|w| w == b"startxref")
            .expect("startxref");
        let num_start = sx + 9;
        // 空白を飛ばして数字列を "0" に置換（桁数を保つため残りを '0' 埋め）
        let mut i = num_start;
        while i < data.len() && matches!(data[i], b' ' | b'\n' | b'\r' | b'\t') {
            i += 1;
        }
        let digit_start = i;
        while i < data.len() && data[i].is_ascii_digit() {
            i += 1;
        }
        assert!(digit_start < i, "expected digits after startxref");
        data[digit_start] = b'0';
        for b in &mut data[digit_start + 1..i] {
            *b = b'0';
        }

        let xref = XRef::parse(&data).expect("parse via recovery");
        assert!(xref.is_recovered(), "should use index_objects recovery");
        let root = xref.fetch(Ref::new(1, 0), &data).expect("catalog");
        assert!(
            matches!(root, Object::Dict(ref d) if matches!(d.get("Type"), Some(Object::Name(n)) if n == "Catalog"))
        );
        assert!(xref.has_usable_root(&data));
    }

    /// startxref 自体が欠落しても trailer 発見で開ける
    #[test]
    fn recovery_missing_startxref() {
        let data = build_one_page_pdf();
        // "startxref\nNNN\n" を削除
        let sx = data
            .windows(9)
            .rposition(|w| w == b"startxref")
            .expect("startxref");
        let mut end = sx + 9;
        while end < data.len() && data[end] != b'%' {
            end += 1;
        }
        let mut broken = Vec::new();
        broken.extend_from_slice(&data[..sx]);
        broken.extend_from_slice(&data[end..]);

        let xref = XRef::parse(&broken).expect("parse without startxref");
        assert!(xref.is_recovered());
        let root = xref.fetch(Ref::new(1, 0), &broken).expect("catalog");
        assert!(matches!(root, Object::Dict(_)));
        assert!(xref.has_usable_root(&broken));
    }

    /// xref テーブルの offset が壊れていても obj 走査でページを列挙できる
    #[test]
    fn recovery_broken_xref_table_enumerates_pages() {
        let mut data = build_one_page_pdf();
        // xref サブセクションの in-use エントリ offset をすべて 0000000000 にする
        // 形式: "0000000NNN 00000 n \n" × 3（obj 1,2,3）
        let xref_kw = data
            .windows(4)
            .position(|w| w == b"xref")
            .expect("xref");
        let mut i = xref_kw;
        let mut patched = 0u32;
        while i + 19 < data.len() && patched < 3 {
            // "nnnnnnnnnn 00000 n \n" = 10桁 + SP + 5桁gen + SP + n + SP
            if data[i].is_ascii_digit()
                && data[i + 10] == b' '
                && data[i + 16] == b' '
                && data[i + 17] == b'n'
            {
                for b in &mut data[i..i + 10] {
                    *b = b'0';
                }
                patched += 1;
                i += 20;
                continue;
            }
            i += 1;
        }
        assert_eq!(patched, 3, "should have zeroed 3 xref offsets");

        let xref = XRef::parse(&data).expect("parse broken xref");
        assert!(xref.is_recovered());
        assert!(xref.has_usable_root(&data));

        // Pages /Kids を辿って 1 ページ分の Page 辞書が取れること
        let pages = xref.fetch(Ref::new(2, 0), &data).expect("pages");
        let Object::Dict(pd) = pages else {
            panic!("expected Pages dict");
        };
        assert!(matches!(pd.get("Count"), Some(Object::Int(1))));
        let kids = pd.get("Kids").expect("Kids");
        let Object::Array(arr) = kids else {
            panic!("expected Kids array");
        };
        assert_eq!(arr.len(), 1);
        let Object::Ref(page_ref) = &arr[0] else {
            panic!("expected page ref");
        };
        let page = xref.fetch(*page_ref, &data).expect("page");
        assert!(
            matches!(page, Object::Dict(ref d) if matches!(d.get("Type"), Some(Object::Name(n)) if n == "Page"))
        );
    }

    /// 正常な PDF では recovery に落ちない
    #[test]
    fn normal_pdf_does_not_use_recovery() {
        let data = build_one_page_pdf();
        let xref = XRef::parse(&data).expect("parse");
        assert!(
            !xref.is_recovered(),
            "intact PDF should use the normal xref path"
        );
        assert!(xref.has_usable_root(&data));
        // 通常経路では obj0 が free として載る
        assert!(matches!(xref.entry(0), Some(XrefEntry::Free)));
    }

    /// 巨大な数字連続で `find_obj_content_end` が O(n²) にならないこと
    #[test]
    fn recovery_giant_digit_run_completes() {
        // startxref を欠落させて recovery に必ず落とす
        let base = build_one_page_pdf();
        let sx = base
            .windows(9)
            .rposition(|w| w == b"startxref")
            .expect("startxref");
        let mut end = sx + 9;
        while end < base.len() && base[end] != b'%' {
            end += 1;
        }

        let mut data = Vec::with_capacity(base.len() + 1_000_000);
        data.extend_from_slice(&base[..sx]);
        // 追加オブジェクト内に改行なしの巨大数字列を仕込む
        data.extend_from_slice(b"4 0 obj\n");
        data.extend(std::iter::repeat(b'9').take(1_000_000));
        data.extend_from_slice(b"\nendobj\n");
        data.extend_from_slice(&base[end..]);

        let start = std::time::Instant::now();
        let xref = XRef::parse(&data).expect("parse via recovery");
        let elapsed = start.elapsed();
        assert!(xref.is_recovered());
        // 修正前は O(n²) で数秒〜数十秒級。マシン差を吸収するため広めに 10 秒で判定
        assert!(
            elapsed.as_secs() < 10,
            "recovery should be near-linear, took {elapsed:?}"
        );
        // 元の Catalog は依然として取れる
        assert!(xref.has_usable_root(&data));
    }

    /// 空白区切りの数字連続でも O(n²) にならないこと
    #[test]
    fn recovery_space_separated_digits_completes() {
        let base = build_one_page_pdf();
        let sx = base
            .windows(9)
            .rposition(|w| w == b"startxref")
            .expect("startxref");
        let mut end = sx + 9;
        while end < base.len() && base[end] != b'%' {
            end += 1;
        }

        let mut data = Vec::with_capacity(base.len() + 1_200_000);
        data.extend_from_slice(&base[..sx]);
        data.extend_from_slice(b"4 0 obj\n");
        // "9 " を大量に並べる。各数字位置で read_scan_token が末尾まで走ると O(n²)
        for _ in 0..500_000 {
            data.extend_from_slice(b"9 ");
        }
        data.extend_from_slice(b"\nendobj\n");
        data.extend_from_slice(&base[end..]);

        let start = std::time::Instant::now();
        let xref = XRef::parse(&data).expect("parse via recovery");
        let elapsed = start.elapsed();
        assert!(xref.is_recovered());
        assert!(
            elapsed.as_secs() < 10,
            "space-separated digits should be near-linear, took {elapsed:?}"
        );
        assert!(xref.has_usable_root(&data));
    }

    /// trailer なしで `xref` キーワードを大量に並べても O(n²) にならないこと
    #[test]
    fn recovery_repeated_xref_without_trailer_completes() {
        let mut data = Vec::with_capacity(1_200_000);
        data.extend_from_slice(b"%PDF-1.4\n");
        // 最小の Catalog / Pages を先に置く
        let o1 = data.len();
        data.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let o2 = data.len();
        data.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n");
        // trailer を欠いた `xref\n` を大量に並べる
        for _ in 0..200_000 {
            data.extend_from_slice(b"xref\n");
        }
        // 末尾には壊れた startxref を置き、recovery に落とす
        data.extend_from_slice(b"startxref\n0\n%%EOF\n");
        let _ = (o1, o2);

        let start = std::time::Instant::now();
        let xref = XRef::parse(&data).expect("parse via recovery");
        let elapsed = start.elapsed();
        assert!(xref.is_recovered());
        assert!(
            elapsed.as_secs() < 10,
            "repeated xref without trailer should be near-linear, took {elapsed:?}"
        );
        // 先頭の Catalog はスキャン序盤で拾えている
        let root = xref.fetch(Ref::new(1, 0), &data).expect("catalog");
        assert!(matches!(root, Object::Dict(_)));
    }
}
