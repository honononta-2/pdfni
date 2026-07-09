//! コンテンツストリーム解釈（パス → edges、テキスト → glyphs）
//!
//! 準拠: pdf.js evaluator.js、specs/04

use std::collections::HashSet;

use crate::extract::{EdgePart, GlyphPart};
use crate::model::Orientation;

use super::filters::decode_stream;
use super::font::{lookup_font, LoadedFont};
use super::lexer::{Lexer, Token};
use super::object::{Dict, Object, Ref};
use super::parser::Resolver;
use super::xref::XRef;

/// 軸平行判定の許容差（pt）
const EPS: f64 = 0.8;

/// Form 解決用の文脈（xref が無い単体テストでは None）
pub(crate) struct ContentCtx<'a> {
    pub data: &'a [u8],
    pub xref: Option<&'a XRef>,
}

/// コンテンツストリームから glyphs と edges を抽出する
pub(crate) fn extract_parts(
    contents: &[u8],
    resources: &Dict,
    view: [f64; 4],
    ctx: ContentCtx<'_>,
) -> (Vec<GlyphPart>, Vec<EdgePart>) {
    let mut glyphs = Vec::new();
    let mut edges = Vec::new();
    let mut visiting = HashSet::new();
    interpret(
        contents,
        resources,
        view,
        &ctx,
        &mut glyphs,
        &mut edges,
        &mut visiting,
        [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    );
    (glyphs, edges)
}

/// コンテンツストリームから軸平行な辺を抽出する
#[cfg(test)]
pub(crate) fn extract_edges(
    contents: &[u8],
    resources: &Dict,
    view: [f64; 4],
    ctx: ContentCtx<'_>,
) -> Vec<EdgePart> {
    extract_parts(contents, resources, view, ctx).1
}

/// 単位行列
fn identity() -> [f64; 6] {
    [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
}

/// m1 ∘ m2（m2 を先に適用）
fn mat_mul(m1: [f64; 6], m2: [f64; 6]) -> [f64; 6] {
    [
        m1[0] * m2[0] + m1[2] * m2[1],
        m1[1] * m2[0] + m1[3] * m2[1],
        m1[0] * m2[2] + m1[2] * m2[3],
        m1[1] * m2[2] + m1[3] * m2[3],
        m1[0] * m2[4] + m1[2] * m2[5] + m1[4],
        m1[1] * m2[4] + m1[3] * m2[5] + m1[5],
    ]
}

/// CTM で点を変換する
fn apply_ctm(m: [f64; 6], x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

/// 行列を (tx, ty) 平行移動する
fn translate_mat(m: [f64; 6], tx: f64, ty: f64) -> [f64; 6] {
    [
        m[0],
        m[1],
        m[2],
        m[3],
        m[0] * tx + m[2] * ty + m[4],
        m[1] * tx + m[3] * ty + m[5],
    ]
}

/// テキスト状態（q/Q で CTM と一緒に保存・復元される。pdf.js の TextState と同様）
#[derive(Clone)]
struct TextState {
    text_matrix: [f64; 6],
    text_line_matrix: [f64; 6],
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    text_h_scale: f64,
    text_rise: f64,
    leading: f64,
    font: Option<LoadedFont>,
}

impl TextState {
    fn new() -> Self {
        Self {
            text_matrix: identity(),
            text_line_matrix: identity(),
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            text_h_scale: 1.0,
            text_rise: 0.0,
            leading: 0.0,
            font: None,
        }
    }

    /// textLineMatrix を (tx,ty) 平行移動して textMatrix に複製
    fn move_text(&mut self, tx: f64, ty: f64) {
        self.text_line_matrix = translate_mat(self.text_line_matrix, tx, ty);
        self.text_matrix = self.text_line_matrix;
    }

    /// 改行（0, -leading）
    fn next_line(&mut self) {
        self.move_text(0.0, -self.leading);
    }

    /// trm = ctm ∘ textMatrix ∘ tsm
    fn text_rendering_matrix(&self, ctm: [f64; 6]) -> [f64; 6] {
        let tsm = [
            self.font_size * self.text_h_scale,
            0.0,
            0.0,
            self.font_size,
            0.0,
            self.text_rise,
        ];
        mat_mul(ctm, mat_mul(self.text_matrix, tsm))
    }
}

/// パスセグメント
#[derive(Debug, Clone)]
enum Seg {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    /// 制御点は捨て、終点のみ保持
    CurveTo(f64, f64),
    Close,
}

struct PathBuilder {
    segs: Vec<Seg>,
    current: Option<(f64, f64)>,
}

impl PathBuilder {
    fn new() -> Self {
        Self {
            segs: Vec::new(),
            current: None,
        }
    }

    fn clear(&mut self) {
        self.segs.clear();
        self.current = None;
    }

    fn move_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::MoveTo(x, y));
        self.current = Some((x, y));
    }

    fn line_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::LineTo(x, y));
        self.current = Some((x, y));
    }

    fn curve_to(&mut self, x: f64, y: f64) {
        self.segs.push(Seg::CurveTo(x, y));
        self.current = Some((x, y));
    }

    fn close(&mut self) {
        self.segs.push(Seg::Close);
        // 現在点は finalize 時にサブパス始点へ戻す
    }

    /// re を move+line×3+close、退化時は1本線に展開する
    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        if w == 0.0 || h == 0.0 {
            self.move_to(x, y);
            self.line_to(x + w, y + h);
            self.close();
        } else {
            self.move_to(x, y);
            self.line_to(x + w, y);
            self.line_to(x + w, y + h);
            self.line_to(x, y + h);
            self.close();
        }
    }
}

/// CTM 適用後の2点を軸平行辺にする（位置は終点側）
fn axis_edge(a: (f64, f64), b: (f64, f64), view: [f64; 4]) -> Option<EdgePart> {
    let (ox, oy) = (view[0], view[3]); // view 左上（PDF 座標）
    let (px, py) = a;
    let (x, y) = b;
    let dx = x - px;
    let dy = y - py;
    if dx.abs() <= EPS && dy.abs() > EPS {
        Some(EdgePart {
            orientation: Orientation::Vertical,
            left: x - ox,
            right: x - ox,
            top: oy - py.max(y),
            bottom: oy - py.min(y),
        })
    } else if dy.abs() <= EPS && dx.abs() > EPS {
        let yy = oy - y;
        Some(EdgePart {
            orientation: Orientation::Horizontal,
            left: px.min(x) - ox,
            right: px.max(x) - ox,
            top: yy,
            bottom: yy,
        })
    } else {
        None
    }
}

/// パスを辺化しバッファを空にする
fn finalize_path(
    path: &mut PathBuilder,
    filled: bool,
    ctm: [f64; 6],
    view: [f64; 4],
    edges: &mut Vec<EdgePart>,
) {
    let segs = std::mem::take(&mut path.segs);
    path.current = None;

    let apply = |x: f64, y: f64| apply_ctm(ctm, x, y);

    let mut sub_start: Option<(f64, f64)> = None;
    let mut prev: Option<(f64, f64)> = None;

    for seg in segs {
        match seg {
            Seg::MoveTo(x, y) => {
                let pt = apply(x, y);
                if filled {
                    if let (Some(s), Some(p)) = (sub_start, prev) {
                        if let Some(e) = axis_edge(p, s, view) {
                            edges.push(e);
                        }
                    }
                }
                sub_start = Some(pt);
                prev = Some(pt);
            }
            Seg::LineTo(x, y) => {
                let pt = apply(x, y);
                if let Some(p) = prev {
                    if let Some(e) = axis_edge(p, pt, view) {
                        edges.push(e);
                    }
                }
                prev = Some(pt);
            }
            Seg::CurveTo(x, y) => {
                prev = Some(apply(x, y));
            }
            Seg::Close => {
                if let (Some(s), Some(p)) = (sub_start, prev) {
                    if let Some(e) = axis_edge(p, s, view) {
                        edges.push(e);
                    }
                    prev = Some(s);
                }
            }
        }
    }

    if filled {
        if let (Some(s), Some(p)) = (sub_start, prev) {
            if let Some(e) = axis_edge(p, s, view) {
                edges.push(e);
            }
        }
    }
}

/// オブジェクトを解決する
fn resolve_obj(obj: &Object, ctx: &ContentCtx<'_>) -> Object {
    match obj {
        Object::Ref(r) => {
            if let Some(xref) = ctx.xref {
                xref.fetch(*r, ctx.data).unwrap_or(Object::Null)
            } else {
                Object::Null
            }
        }
        other => other.clone(),
    }
}

struct FetchResolver<'a> {
    xref: Option<&'a XRef>,
    data: &'a [u8],
}

impl Resolver for FetchResolver<'_> {
    fn resolve(&self, r: Ref) -> Option<Object> {
        self.xref.and_then(|x| x.fetch(r, self.data).ok())
    }
}

fn as_f64(obj: &Object) -> Option<f64> {
    match obj {
        Object::Int(n) => Some(*n as f64),
        Object::Real(n) => Some(*n),
        _ => None,
    }
}

/// スタック末尾 n 個の数値を順序どおり取り出す
fn take_nums(stack: &mut Vec<Object>, n: usize) -> Option<Vec<f64>> {
    if stack.len() < n {
        return None;
    }
    let start = stack.len() - n;
    let mut out = Vec::with_capacity(n);
    for o in &stack[start..] {
        out.push(as_f64(o)?);
    }
    stack.truncate(start);
    Some(out)
}

fn take_name(stack: &mut Vec<Object>) -> Option<String> {
    match stack.pop() {
        Some(Object::Name(n)) => Some(n),
        Some(other) => {
            stack.push(other);
            None
        }
        None => None,
    }
}

/// 配列リテラルをパースする
fn parse_array(lexer: &mut Lexer<'_>) -> Object {
    let mut arr = Vec::new();
    loop {
        match lexer.next_token() {
            Token::ArrayEnd | Token::Eof => break,
            Token::ArrayStart => arr.push(parse_array(lexer)),
            Token::DictStart => arr.push(parse_dict(lexer)),
            Token::Int(n) => arr.push(Object::Int(n)),
            Token::Real(r) => arr.push(Object::Real(r)),
            Token::Str(s) => arr.push(Object::Str(s)),
            Token::Name(n) => arr.push(Object::Name(n)),
            Token::Cmd(s) => match s.as_str() {
                "true" => arr.push(Object::Bool(true)),
                "false" => arr.push(Object::Bool(false)),
                "null" => arr.push(Object::Null),
                "R" => {
                    // n g R
                    if arr.len() >= 2 {
                        let g = arr.pop();
                        let n = arr.pop();
                        if let (Some(Object::Int(num)), Some(Object::Int(generation))) = (n, g) {
                            if (0..=i64::from(u32::MAX)).contains(&num)
                                && (0..=i64::from(u16::MAX)).contains(&generation)
                            {
                                arr.push(Object::Ref(Ref::new(num as u32, generation as u16)));
                                continue;
                            }
                        }
                        // 復元できない場合は捨てる
                    }
                }
                _ => {} // 配列内の未知コマンドは無視
            },
            Token::DictEnd | Token::BraceStart | Token::BraceEnd => {}
        }
    }
    Object::Array(arr)
}

/// 辞書リテラルをパースする
fn parse_dict(lexer: &mut Lexer<'_>) -> Object {
    let mut dict = Dict::new();
    loop {
        match lexer.next_token() {
            Token::DictEnd | Token::Eof => break,
            Token::Name(key) => {
                let value = match lexer.next_token() {
                    Token::ArrayStart => parse_array(lexer),
                    Token::DictStart => parse_dict(lexer),
                    Token::Int(n) => Object::Int(n),
                    Token::Real(r) => Object::Real(r),
                    Token::Str(s) => Object::Str(s),
                    Token::Name(n) => Object::Name(n),
                    Token::Cmd(s) => match s.as_str() {
                        "true" => Object::Bool(true),
                        "false" => Object::Bool(false),
                        "null" => Object::Null,
                        _ => Object::Null,
                    },
                    Token::Eof | Token::DictEnd | Token::ArrayEnd => break,
                    _ => Object::Null,
                };
                dict.set(key, value);
            }
            Token::ArrayStart => {
                let _ = parse_array(lexer);
            }
            Token::DictStart => {
                let _ = parse_dict(lexer);
            }
            _ => {}
        }
    }
    Object::Dict(dict)
}

/// BI...EI を `data` 上でスキップし lexer 位置を更新する
fn skip_inline_image_in(data: &[u8], lexer: &mut Lexer<'_>) {
    loop {
        match lexer.next_token() {
            Token::Cmd(s) if s == "ID" => break,
            Token::Eof => return,
            _ => {}
        }
    }

    let mut pos = lexer.byte_pos();
    // ID 直後の空白を1回分スキップ（改行含む）
    while pos < data.len() && is_pdf_ws(data[pos]) {
        pos += 1;
    }

    // 「空白に続く EI、直後が空白または終端」を探す
    let mut i = pos;
    while i + 2 <= data.len() {
        if data[i] == b'E' && data[i + 1] == b'I' {
            let prev_ws = i == 0 || is_pdf_ws(data[i - 1]);
            // 画像データ開始直後の EI は i==pos で prev がデータ先頭。
            // 仕様どおり「空白に続く」なので、pos より前の空白（ID 後）も認める:
            // i == pos のとき、pos 直前が空白なら OK（すでにスキップ済みなので true 扱い）
            let prev_ok = if i == pos {
                // ID 後空白の直後に EI → 空画像。許容する
                true
            } else {
                prev_ws
            };
            let next_ok = i + 2 >= data.len() || is_pdf_ws(data[i + 2]);
            if prev_ok && next_ok {
                // EI の直後へ
                let after = i + 2;
                lexer.set_pos(after);
                return;
            }
        }
        i += 1;
    }
    // 見つからなければ末尾へ
    lexer.set_pos(data.len());
}

fn is_pdf_ws(b: u8) -> bool {
    matches!(b, 0x00 | 0x09 | 0x0a | 0x0c | 0x0d | 0x20)
}

/// ストリーム本文を解釈して glyphs / edges に追記する
fn interpret(
    contents: &[u8],
    resources: &Dict,
    view: [f64; 4],
    ctx: &ContentCtx<'_>,
    glyphs: &mut Vec<GlyphPart>,
    edges: &mut Vec<EdgePart>,
    visiting: &mut HashSet<(u32, u16)>,
    ctm0: [f64; 6],
) {
    let mut lexer = Lexer::new(contents);
    let mut stack: Vec<Object> = Vec::new();
    let mut ctm = ctm0;
    let mut gstack: Vec<([f64; 6], TextState)> = Vec::new();
    let mut path = PathBuilder::new();
    let mut text = TextState::new();

    loop {
        let token = lexer.next_token();
        match token {
            Token::Eof => break,
            Token::Int(n) => stack.push(Object::Int(n)),
            Token::Real(r) => stack.push(Object::Real(r)),
            Token::Str(s) => stack.push(Object::Str(s)),
            Token::Name(n) => stack.push(Object::Name(n)),
            Token::ArrayStart => stack.push(parse_array(&mut lexer)),
            Token::DictStart => stack.push(parse_dict(&mut lexer)),
            Token::ArrayEnd | Token::DictEnd | Token::BraceStart | Token::BraceEnd => {
                // 単独の区切りは無視
            }
            Token::Cmd(op) => {
                match op.as_str() {
                    "true" => stack.push(Object::Bool(true)),
                    "false" => stack.push(Object::Bool(false)),
                    "null" => stack.push(Object::Null),
                    "R" => {
                        if let Some(nums) = take_nums(&mut stack, 2) {
                            let num = nums[0] as i64;
                            let generation = nums[1] as i64;
                            if (0..=i64::from(u32::MAX)).contains(&num)
                                && (0..=i64::from(u16::MAX)).contains(&generation)
                            {
                                stack.push(Object::Ref(Ref::new(num as u32, generation as u16)));
                            }
                        }
                    }
                    "BI" => {
                        stack.clear();
                        skip_inline_image_in(contents, &mut lexer);
                    }
                    other => {
                        dispatch_op(
                            other,
                            &mut stack,
                            &mut ctm,
                            &mut gstack,
                            &mut path,
                            &mut text,
                            resources,
                            view,
                            ctx,
                            glyphs,
                            edges,
                            visiting,
                        );
                        stack.clear();
                    }
                }
            }
        }
    }
}

fn dispatch_op(
    op: &str,
    stack: &mut Vec<Object>,
    ctm: &mut [f64; 6],
    gstack: &mut Vec<([f64; 6], TextState)>,
    path: &mut PathBuilder,
    text: &mut TextState,
    resources: &Dict,
    view: [f64; 4],
    ctx: &ContentCtx<'_>,
    glyphs: &mut Vec<GlyphPart>,
    edges: &mut Vec<EdgePart>,
    visiting: &mut HashSet<(u32, u16)>,
) {
    match op {
        // グラフィックス状態（テキスト状態も保存・復元する。アンダーフローは無視）
        "q" => gstack.push((*ctm, text.clone())),
        "Q" => {
            if let Some((m, t)) = gstack.pop() {
                *ctm = m;
                *text = t;
            }
        }
        "cm" => {
            if let Some(n) = take_nums(stack, 6) {
                let m = [n[0], n[1], n[2], n[3], n[4], n[5]];
                *ctm = mat_mul(*ctm, m);
            }
        }

        // パス構築
        "m" => {
            if let Some(n) = take_nums(stack, 2) {
                path.move_to(n[0], n[1]);
            }
        }
        "l" => {
            if let Some(n) = take_nums(stack, 2) {
                path.line_to(n[0], n[1]);
            }
        }
        "c" => {
            if let Some(n) = take_nums(stack, 6) {
                path.curve_to(n[4], n[5]);
            }
        }
        "v" => {
            if let Some(n) = take_nums(stack, 4) {
                path.curve_to(n[2], n[3]);
            }
        }
        "y" => {
            if let Some(n) = take_nums(stack, 4) {
                path.curve_to(n[2], n[3]);
            }
        }
        "h" => path.close(),
        "re" => {
            if let Some(n) = take_nums(stack, 4) {
                path.rect(n[0], n[1], n[2], n[3]);
            }
        }

        // ペイント（close 系は確定前に閉じを追加）
        "S" => finalize_path(path, false, *ctm, view, edges),
        "s" => {
            path.close();
            finalize_path(path, false, *ctm, view, edges);
        }
        "f" | "F" | "f*" => finalize_path(path, true, *ctm, view, edges),
        "B" | "B*" => finalize_path(path, true, *ctm, view, edges),
        "b" | "b*" => {
            path.close();
            finalize_path(path, true, *ctm, view, edges);
        }
        "n" => path.clear(),

        // クリップ・線属性は無視
        "W" | "W*" | "w" | "d" | "J" | "j" | "M" => {}

        // Form XObject
        "Do" => {
            if let Some(name) = take_name(stack) {
                do_form_xobject(&name, *ctm, resources, view, ctx, glyphs, edges, visiting);
            }
        }

        // テキスト状態
        "BT" => {
            text.text_matrix = identity();
            text.text_line_matrix = identity();
        }
        "ET" => {}
        "Tf" => {
            // /Name size Tf → stack [Name, size]
            if let Some(sz) = take_nums(stack, 1) {
                if let Some(name) = take_name(stack) {
                    text.font_size = sz[0];
                    text.font = Some(lookup_font(resources, &name, ctx.data, ctx.xref));
                }
            }
        }
        "Tc" => {
            if let Some(n) = take_nums(stack, 1) {
                text.char_spacing = n[0];
            }
        }
        "Tw" => {
            if let Some(n) = take_nums(stack, 1) {
                text.word_spacing = n[0];
            }
        }
        "Tz" => {
            if let Some(n) = take_nums(stack, 1) {
                text.text_h_scale = n[0] / 100.0;
            }
        }
        "TL" => {
            if let Some(n) = take_nums(stack, 1) {
                text.leading = n[0];
            }
        }
        "Ts" => {
            if let Some(n) = take_nums(stack, 1) {
                text.text_rise = n[0];
            }
        }
        "Tr" => {
            // レンダリングモードは無視（不可視も出力）
            let _ = take_nums(stack, 1);
        }
        "Td" => {
            if let Some(n) = take_nums(stack, 2) {
                text.move_text(n[0], n[1]);
            }
        }
        "TD" => {
            if let Some(n) = take_nums(stack, 2) {
                text.leading = -n[1];
                text.move_text(n[0], n[1]);
            }
        }
        "Tm" => {
            if let Some(n) = take_nums(stack, 6) {
                let m = [n[0], n[1], n[2], n[3], n[4], n[5]];
                text.text_matrix = m;
                text.text_line_matrix = m;
            }
        }
        "T*" => text.next_line(),
        "Tj" => {
            if let Some(Object::Str(s)) = stack.pop() {
                show_text_bytes(text, &s, 0.0, *ctm, view, glyphs);
            }
        }
        "TJ" => {
            if let Some(Object::Array(arr)) = stack.pop() {
                show_text_array(text, &arr, *ctm, view, glyphs);
            }
        }
        "'" => {
            // 改行してから Tj
            if let Some(Object::Str(s)) = stack.pop() {
                text.next_line();
                show_text_bytes(text, &s, 0.0, *ctm, view, glyphs);
            }
        }
        "\"" => {
            // aw ac string " → Tw Tc T* Tj
            if let Some(Object::Str(s)) = stack.pop() {
                if let Some(n) = take_nums(stack, 2) {
                    text.word_spacing = n[0];
                    text.char_spacing = n[1];
                    text.next_line();
                    show_text_bytes(text, &s, 0.0, *ctm, view, glyphs);
                }
            }
        }

        // その他は無視
        _ => {}
    }
}

/// TJ 配列の表示
fn show_text_array(
    text: &mut TextState,
    arr: &[Object],
    ctm: [f64; 6],
    view: [f64; 4],
    glyphs: &mut Vec<GlyphPart>,
) {
    let mut buf: Vec<u8> = Vec::new();
    for item in arr {
        match item {
            Object::Str(s) => buf.extend_from_slice(s),
            Object::Int(n) => {
                let n = *n as f64;
                if n != 0.0 {
                    let extra = -n / 1000.0 * text.font_size;
                    show_text_bytes(text, &buf, extra, ctm, view, glyphs);
                    buf.clear();
                }
            }
            Object::Real(n) => {
                if *n != 0.0 {
                    let extra = -n / 1000.0 * text.font_size;
                    show_text_bytes(text, &buf, extra, ctm, view, glyphs);
                    buf.clear();
                }
            }
            _ => {}
        }
    }
    if !buf.is_empty() {
        show_text_bytes(text, &buf, 0.0, ctm, view, glyphs);
    }
}

/// バイト列をグリフに展開して出力し textMatrix を進める
///
/// `extra_spacing` は最終グリフの字送りに加算（テキスト空間、hScale 前）
fn show_text_bytes(
    text: &mut TextState,
    bytes: &[u8],
    extra_spacing: f64,
    ctm: [f64; 6],
    view: [f64; 4],
    glyphs: &mut Vec<GlyphPart>,
) {
    let Some(font) = text.font.clone() else {
        // フォント未設定時は extra だけの平行移動（TJ 数値のみ）
        if extra_spacing != 0.0 {
            let dx = extra_spacing * text.text_h_scale;
            text.text_matrix = translate_mat(text.text_matrix, dx, 0.0);
        }
        return;
    };

    if bytes.is_empty() {
        if extra_spacing != 0.0 {
            let dx = (text.char_spacing + extra_spacing) * text.text_h_scale;
            text.text_matrix = translate_mat(text.text_matrix, dx, 0.0);
        }
        return;
    }

    let glyph_list = font.chars_to_glyphs(bytes);
    let count = glyph_list.len();
    let (view_left, view_top) = (view[0], view[3]);
    let ascent = font.ascent();
    let descent = font.descent();

    for (i, (unicode, width, is_space)) in glyph_list.into_iter().enumerate() {
        let extra = if i + 1 == count { extra_spacing } else { 0.0 };

        let trm = text.text_rendering_matrix(ctm);
        let (ox, oy) = (trm[4], trm[5]);
        let font_size_dev = (trm[2] * trm[2] + trm[3] * trm[3]).sqrt();
        let dlen = (trm[0] * trm[0] + trm[1] * trm[1]).sqrt();
        let (ux, _uy) = if dlen > 0.0 {
            (trm[0] / dlen, trm[1] / dlen)
        } else {
            (1.0, 0.0)
        };

        // scaledDim = 幅/1000 × fontSize × hScale
        let scaled_dim = width / 1000.0 * text.font_size * text.text_h_scale;
        let mut spacing = text.char_spacing;
        if is_space {
            spacing += text.word_spacing;
        }
        // bbox 用（Tc/Tw は含む、TJ 数値 extra は位置のみ）
        let bbox_advance = scaled_dim + spacing * text.text_h_scale;
        // textMatrix の前進（TJ extra を加算）
        let advance = bbox_advance + extra * text.text_h_scale;

        // デバイス空間での実効字送り幅（bbox 用。進行方向への符号付き長）
        let end_tm = translate_mat(text.text_matrix, bbox_advance, 0.0);
        let end_trm = {
            let tsm = [
                text.font_size * text.text_h_scale,
                0.0,
                0.0,
                text.font_size,
                0.0,
                text.text_rise,
            ];
            mat_mul(ctm, mat_mul(end_tm, tsm))
        };
        let (ex, ey) = (end_trm[4], end_trm[5]);
        let uy = if dlen > 0.0 { trm[1] / dlen } else { 0.0 };
        let device_w = (ex - ox) * ux + (ey - oy) * uy;

        // コードポイントごとに分割し幅を等分
        let chars: Vec<char> = unicode.chars().collect();
        let nch = chars.len().max(1);
        let per_w = device_w / nch as f64;

        let top_pdf = oy + ascent * font_size_dev;
        let bot_pdf = oy + descent * font_size_dev;
        let top = view_top - top_pdf;
        let bottom = view_top - bot_pdf;

        for (k, ch) in chars.into_iter().enumerate() {
            if ch == '\n' || ch == '\r' || ch == '\t' {
                continue;
            }
            let gx0 = ox + ux * per_w * k as f64;
            let gx1 = ox + ux * per_w * (k as f64 + 1.0);
            glyphs.push(GlyphPart {
                ch,
                left: gx0.min(gx1) - view_left,
                right: gx0.max(gx1) - view_left,
                top,
                bottom,
            });
        }

        text.text_matrix = translate_mat(text.text_matrix, advance, 0.0);
    }
}

fn do_form_xobject(
    name: &str,
    ctm: [f64; 6],
    resources: &Dict,
    view: [f64; 4],
    ctx: &ContentCtx<'_>,
    glyphs: &mut Vec<GlyphPart>,
    edges: &mut Vec<EdgePart>,
    visiting: &mut HashSet<(u32, u16)>,
) {
    let Some(xo_obj) = resources.get("XObject") else {
        return;
    };
    let xo = resolve_obj(xo_obj, ctx);
    let Object::Dict(xo_dict) = xo else {
        return;
    };
    let Some(entry) = xo_dict.get(name) else {
        return;
    };

    // 循環検出（参照番号）
    let ref_id = match entry {
        Object::Ref(r) => Some(*r),
        _ => None,
    };
    if let Some(r) = ref_id {
        if !visiting.insert((r.num, r.generation)) {
            return;
        }
    }

    let resolved = resolve_obj(entry, ctx);
    let Object::Stream(stream) = resolved else {
        if let Some(r) = ref_id {
            visiting.remove(&(r.num, r.generation));
        }
        return;
    };

    // Subtype = Form のみ
    let subtype = stream
        .dict
        .get("Subtype")
        .map(|o| resolve_obj(o, ctx))
        .unwrap_or(Object::Null);
    let is_form = matches!(&subtype, Object::Name(n) if n == "Form");
    if !is_form {
        if let Some(r) = ref_id {
            visiting.remove(&(r.num, r.generation));
        }
        return;
    }

    // /Matrix を CTM に合成（無ければ単位行列）
    let form_mat = parse_matrix(stream.dict.get("Matrix"), ctx).unwrap_or_else(identity);

    // /Resources（無ければ親を継承）
    let form_res = match stream.dict.get("Resources") {
        Some(obj) => match resolve_obj(obj, ctx) {
            Object::Dict(d) => d,
            _ => resources.clone(),
        },
        None => resources.clone(),
    };

    // 本文デコード
    let resolver = FetchResolver {
        xref: ctx.xref,
        data: ctx.data,
    };
    let body = match decode_stream(ctx.data, &stream, &resolver, ctx.xref.and_then(|x| x.cipher())) {
        Ok(b) => b,
        // インライン Stream は data が本文そのものの場合あり
        Err(_) => {
            // start/length が ctx.data を指している前提で再試行
            let end = stream.start.saturating_add(stream.length);
            if end <= ctx.data.len() {
                ctx.data[stream.start..end].to_vec()
            } else {
                if let Some(r) = ref_id {
                    visiting.remove(&(r.num, r.generation));
                }
                return;
            }
        }
    };

    // /Matrix を CTM に合成して本文を実行（テキスト状態は Form 内で独立）
    interpret(
        &body,
        &form_res,
        view,
        ctx,
        glyphs,
        edges,
        visiting,
        mat_mul(ctm, form_mat),
    );

    if let Some(r) = ref_id {
        visiting.remove(&(r.num, r.generation));
    }
}

fn parse_matrix(obj: Option<&Object>, ctx: &ContentCtx<'_>) -> Option<[f64; 6]> {
    let obj = resolve_obj(obj?, ctx);
    let Object::Array(arr) = obj else {
        return None;
    };
    if arr.len() < 6 {
        return None;
    }
    Some([
        as_f64(&arr[0])?,
        as_f64(&arr[1])?,
        as_f64(&arr[2])?,
        as_f64(&arr[3])?,
        as_f64(&arr[4])?,
        as_f64(&arr[5])?,
    ])
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader_rs::object::Stream;

    fn edges_of(contents: &[u8], view: [f64; 4]) -> Vec<EdgePart> {
        extract_edges(
            contents,
            &Dict::new(),
            view,
            ContentCtx {
                data: &[],
                xref: None,
            },
        )
    }

    fn default_view() -> [f64; 4] {
        [0.0, 0.0, 200.0, 200.0]
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    fn find_edge<'a>(
        edges: &'a [EdgePart],
        orient: Orientation,
        left: f64,
        right: f64,
        top: f64,
        bottom: f64,
    ) -> bool {
        edges.iter().any(|e| {
            e.orientation == orient
                && approx(e.left, left)
                && approx(e.right, right)
                && approx(e.top, top)
                && approx(e.bottom, bottom)
        })
    }

    /// re + S で4辺
    #[test]
    fn re_stroke_four_edges() {
        // 矩形 (10,20)-(60,70) → w=50 h=50。view top=200
        // PDF y: bottom-up。top-down: top = 200-y
        // 下辺 y=20 → top=180、上辺 y=70 → top=130
        // 左 x=10、右 x=60
        let edges = edges_of(b"10 20 50 50 re S", default_view());
        assert_eq!(edges.len(), 4, "expected 4 edges, got {edges:?}");

        // bottom: (10,20)-(60,20) horizontal y_end=20 → top=180
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            10.0,
            60.0,
            180.0,
            180.0
        ));
        // right: (60,20)-(60,70) vertical x_end=60
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            60.0,
            60.0,
            130.0,
            180.0
        ));
        // top: (60,70)-(10,70) horizontal y_end=70 → top=130
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            10.0,
            60.0,
            130.0,
            130.0
        ));
        // left close: (10,70)-(10,20) vertical x_end=10
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            10.0,
            10.0,
            130.0,
            180.0
        ));
    }

    /// 退化矩形 w=0 は線分と閉じ線で同一の縦辺が2本になる（pdf.js ルートと同じ挙動。
    /// 重複辺は検出コアが吸収する）
    #[test]
    fn degenerate_rect_zero_width() {
        // re x y 0 h → move (x,y) line (x, y+h) close
        // 10 20 0 50 → 縦線 x=10, y=20..70
        let edges = edges_of(b"10 20 0 50 re S", default_view());
        assert_eq!(edges.len(), 2, "expected 2 identical edges, got {edges:?}");
        for e in &edges {
            assert_eq!(e.orientation, Orientation::Vertical);
            assert!(approx(e.left, 10.0));
            assert!(approx(e.top, 130.0));
            assert!(approx(e.bottom, 180.0));
        }
    }

    /// cm で平行移動した線分
    #[test]
    fn cm_translates_line() {
        // 0 0 m 100 0 l を (10, 20) 平行移動
        let edges = edges_of(b"1 0 0 1 10 20 cm 0 0 m 100 0 l S", default_view());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].orientation, Orientation::Horizontal);
        // 終点 y = 20 → top = 200-20 = 180
        assert!(approx(edges[0].top, 180.0));
        assert!(approx(edges[0].left, 10.0));
        assert!(approx(edges[0].right, 110.0));
    }

    /// q/Q で CTM が戻る
    #[test]
    fn q_q_restores_ctm() {
        // q 内で平行移動して線、Q 後に別の線
        let contents = b"\
q
1 0 0 1 50 0 cm
0 100 m 10 100 l S
Q
0 50 m 10 50 l S
";
        let edges = edges_of(contents, default_view());
        assert_eq!(edges.len(), 2, "got {edges:?}");
        // 1本目: x=50..60, y=100 → top=100
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            50.0,
            60.0,
            100.0,
            100.0
        ));
        // 2本目: x=0..10, y=50 → top=150（CTM が戻っている）
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            0.0,
            10.0,
            150.0,
            150.0
        ));
    }

    /// 塗りの三角形で暗黙閉じ線が辺化（軸平行分のみ）
    #[test]
    fn fill_triangle_implicit_close() {
        // (0,0) → (10,0) → (10,10) → 暗黙閉じ (10,10)→(0,0) は斜め → 捨て
        // 辺: 水平 (0,0)-(10,0)、垂直 (10,0)-(10,10)
        let edges = edges_of(b"0 0 m 10 0 l 10 10 l f", default_view());
        assert_eq!(edges.len(), 2, "got {edges:?}");
        assert!(find_edge(
            &edges,
            Orientation::Horizontal,
            0.0,
            10.0,
            200.0,
            200.0
        ));
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            10.0,
            10.0,
            190.0,
            200.0
        ));
    }

    /// 塗りの軸平行閉じ: 3点で暗黙閉じが水平になる
    #[test]
    fn fill_implicit_close_axis() {
        // (0,0)-(10,0)-(10,0) wait: (0,5)-(10,5)-(10,15) close implicit (10,15)-(0,5) 斜め
        // (0,0)-(20,0)-(20,10)-(0,10) without h, fill → 暗黙閉じが左縦辺
        let edges = edges_of(b"0 0 m 20 0 l 20 10 l 0 10 l f", default_view());
        // 3 明示辺 + 1 暗黙閉じ = 4
        assert_eq!(edges.len(), 4, "got {edges:?}");
        assert!(find_edge(
            &edges,
            Orientation::Vertical,
            0.0,
            0.0,
            190.0,
            200.0
        ));
    }

    /// 斜め線は捨て、EPS 以内のズレは軸平行
    #[test]
    fn diagonal_dropped_eps_accepted() {
        // 斜め
        let edges = edges_of(b"0 0 m 10 10 l S", default_view());
        assert!(edges.is_empty());

        // dy=0.5 < 0.8 → horizontal
        let edges = edges_of(b"0 0 m 10 0.5 l S", default_view());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].orientation, Orientation::Horizontal);
        // 終点 y=0.5 → top = 200-0.5 = 199.5
        assert!(approx(edges[0].top, 199.5));

        // dx=0.5 < 0.8 → vertical
        let edges = edges_of(b"0 0 m 0.5 10 l S", default_view());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].orientation, Orientation::Vertical);
    }

    /// n は辺にしない
    #[test]
    fn end_path_n_no_edges() {
        let edges = edges_of(b"0 0 100 50 re n", default_view());
        assert!(edges.is_empty());
        // W n も同様
        let edges = edges_of(b"0 0 100 50 re W n", default_view());
        assert!(edges.is_empty());
    }

    /// Form XObject の Matrix 合成と再帰
    #[test]
    fn form_xobject_matrix_and_recurse() {
        // Form 本文: 原点から水平線 0 0 m 20 0 l S
        // Matrix: 平行移動 (30, 40)
        let form_body = b"0 0 m 20 0 l S";
        let mut form_dict = Dict::new();
        form_dict.set("Subtype", Object::Name("Form".into()));
        form_dict.set(
            "Matrix",
            Object::Array(vec![
                Object::Int(1),
                Object::Int(0),
                Object::Int(0),
                Object::Int(1),
                Object::Int(30),
                Object::Int(40),
            ]),
        );
        form_dict.set("Length", Object::Int(form_body.len() as i64));
        let stream = Stream::new(form_dict, 0, form_body.len());

        let mut xo = Dict::new();
        xo.set("Fm1", Object::Stream(stream));
        let mut resources = Dict::new();
        resources.set("XObject", Object::Dict(xo));

        let edges = extract_edges(
            b"/Fm1 Do",
            &resources,
            default_view(),
            ContentCtx {
                data: form_body,
                xref: None,
            },
        );
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].orientation, Orientation::Horizontal);
        // 変換後 (30,40)-(50,40)、top = 200-40 = 160
        assert!(approx(edges[0].left, 30.0));
        assert!(approx(edges[0].right, 50.0));
        assert!(approx(edges[0].top, 160.0));
    }

    /// BI...EI スキップ後の演算子が処理される
    #[test]
    fn bi_ei_skip_then_path() {
        // インライン画像の後に水平線
        let contents = b"BI /W 1 /H 1 /CS /G ID \x00 EI 0 100 m 50 100 l S";
        let edges = edges_of(contents, default_view());
        assert_eq!(edges.len(), 1, "got {edges:?}");
        assert_eq!(edges[0].orientation, Orientation::Horizontal);
        assert!(approx(edges[0].left, 0.0));
        assert!(approx(edges[0].right, 50.0));
        assert!(approx(edges[0].top, 100.0));
    }

    /// view 原点が非ゼロのときの left/top
    #[test]
    fn view_origin_nonzero() {
        // view = [10, 20, 210, 220] → 原点左上 PDF (10, 220)
        // 線 (30, 40)-(80, 40)
        // left = 30-10 = 20, right = 80-10 = 70
        // top = 220-40 = 180
        let view = [10.0, 20.0, 210.0, 220.0];
        let edges = edges_of(b"30 40 m 80 40 l S", view);
        assert_eq!(edges.len(), 1);
        assert!(approx(edges[0].left, 20.0));
        assert!(approx(edges[0].right, 70.0));
        assert!(approx(edges[0].top, 180.0));
        assert!(approx(edges[0].bottom, 180.0));

        // 縦線 (30, 40)-(30, 90)
        // left = 30-10 = 20
        // top = 220-90 = 130, bottom = 220-40 = 180
        let edges = edges_of(b"30 40 m 30 90 l S", view);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].orientation, Orientation::Vertical);
        assert!(approx(edges[0].left, 20.0));
        assert!(approx(edges[0].top, 130.0));
        assert!(approx(edges[0].bottom, 180.0));
    }

    /// stroke のみでは暗黙閉じしない
    #[test]
    fn stroke_no_implicit_close() {
        let edges = edges_of(b"0 0 m 20 0 l 20 10 l 0 10 l S", default_view());
        // 3 辺のみ（左辺は閉じなし）
        assert_eq!(edges.len(), 3, "got {edges:?}");
    }

    // -----------------------------------------------------------------------
    // テキスト（glyphs）
    // -----------------------------------------------------------------------

    fn glyphs_of(contents: &[u8], view: [f64; 4]) -> Vec<GlyphPart> {
        extract_parts(
            contents,
            &Dict::new(),
            view,
            ContentCtx {
                data: &[],
                xref: None,
            },
        )
        .0
    }

    /// スタブ: 幅 500/1000、fontSize に対する字送り = 0.5 * fontSize（hScale=1）
    fn stub_advance(font_size: f64) -> f64 {
        0.5 * font_size
    }

    /// Td / Tm での原点移動と bbox の位置
    #[test]
    fn text_td_tm_origin_and_bbox() {
        // Tm で (100, 50) に置き "A"、fontSize=20
        // 原点 PDF (100, 50)、幅 10、ascent=0.88 descent=-0.12
        let contents = b"BT /F1 20 Tf 1 0 0 1 100 50 Tm (A) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ch, 'A');
        assert!(approx(g[0].left, 100.0), "left={}", g[0].left);
        assert!(approx(g[0].right, 110.0), "right={}", g[0].right);
        // top = 200 - (50 + 0.88*20) = 200 - 67.6 = 132.4
        // bottom = 200 - (50 + (-0.12)*20) = 200 - 47.6 = 152.4
        assert!(approx(g[0].top, 132.4), "top={}", g[0].top);
        assert!(approx(g[0].bottom, 152.4), "bottom={}", g[0].bottom);

        // Td で (30, 40) 移動（BT 後の単位行列から）
        let contents = b"BT /F1 10 Tf 30 40 Td (B) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ch, 'B');
        assert!(approx(g[0].left, 30.0));
        assert!(approx(g[0].right, 30.0 + stub_advance(10.0)));
        // baseline y=40, top = 200 - (40+8.8) = 151.2
        assert!(approx(g[0].top, 151.2));
        assert!(approx(g[0].bottom, 200.0 - (40.0 - 1.2)));
    }

    /// TJ の数値調整が次グリフの x に効く
    #[test]
    fn text_tj_number_shifts_next_glyph() {
        // [(A) 200 (B)] fontSize=10 → A 幅 5、数値 200 → extra = -200/1000*10 = -2
        // B の left = 0 + 5 + (-2) = 3
        let contents = b"BT /F1 10 Tf 1 0 0 1 0 100 Tm [(A) 200 (B)] TJ ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 2, "got {g:?}");
        assert_eq!(g[0].ch, 'A');
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 5.0));
        assert_eq!(g[1].ch, 'B');
        assert!(approx(g[1].left, 3.0), "left={}", g[1].left);
        assert!(approx(g[1].right, 8.0), "right={}", g[1].right);
    }

    /// Tc / Tz の字送り、Tw が空白のみ
    #[test]
    fn text_tc_tz_tw_advance() {
        // Tc=2, fontSize=10, "AB" → each glyph advance = 5 + 2 = 7
        let contents = b"BT /F1 10 Tf 2 Tc 1 0 0 1 0 50 Tm (AB) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 2);
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 7.0), "right={}", g[0].right);
        assert!(approx(g[1].left, 7.0));
        assert!(approx(g[1].right, 14.0));

        // Tz 50% → hScale=0.5, scaledDim = 0.5*10*0.5 = 2.5, Tc=0
        let contents = b"BT /F1 10 Tf 50 Tz 1 0 0 1 0 50 Tm (A) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 2.5), "right={}", g[0].right);

        // Tw=4, "A B" → A advance 5, space advance 5+4=9, B after that
        let contents = b"BT /F1 10 Tf 4 Tw 1 0 0 1 0 50 Tm (A B) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 3);
        assert_eq!(g[0].ch, 'A');
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 5.0)); // Tw は空白以外に効かない
        assert_eq!(g[1].ch, ' ');
        assert!(approx(g[1].left, 5.0));
        assert!(approx(g[1].right, 14.0), "space right={}", g[1].right);
        assert_eq!(g[2].ch, 'B');
        assert!(approx(g[2].left, 14.0));
    }

    /// q/Q でテキスト状態（Tw 等）が保存・復元される
    #[test]
    fn text_state_restored_by_grestore() {
        // q 内で Tw=100 を設定し Q で復元 → 以降の空白に Tw が効かない
        let contents =
            b"q BT /F1 10 Tf 100 Tw ET Q BT /F1 10 Tf 1 0 0 1 0 50 Tm (A B) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 3, "got {g:?}");
        assert_eq!(g[1].ch, ' ');
        assert!(
            approx(g[1].right - g[1].left, 5.0),
            "space width={}",
            g[1].right - g[1].left
        );
    }

    /// ' と " の改行・設定
    #[test]
    fn text_quote_operators() {
        // TL=12, Td to (10,100), "A" then ' "B" → B on next line y=100-12=88
        let contents = b"BT /F1 10 Tf 12 TL 10 100 Td (A) Tj (B) ' ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].ch, 'A');
        // baseline 100 → bottom = 200 - (100 - 1.2) = 101.2
        assert!(approx(g[0].left, 10.0));
        assert!(approx(g[0].bottom, 101.2), "bottom={}", g[0].bottom);
        assert_eq!(g[1].ch, 'B');
        // baseline 88 → bottom = 200 - (88 - 1.2) = 113.2
        assert!(approx(g[1].left, 10.0));
        assert!(approx(g[1].bottom, 113.2), "bottom={}", g[1].bottom);

        // " : 3 1 (C) " → Tw=3 Tc=1, next line, show C
        // after previous? standalone: BT, TL, Td, then 3 1 (C) "
        let contents = b"BT /F1 10 Tf 12 TL 0 100 Td 3 1 (C) \" ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].ch, 'C');
        // next line: y=100-12=88, advance = 5 + 1 = 6 (Tc=1)
        assert!(approx(g[0].left, 0.0));
        assert!(approx(g[0].right, 6.0), "right={}", g[0].right);
        assert!(approx(g[0].bottom, 113.2));
    }

    /// cm とテキスト行列の合成（拡大時の bbox）
    #[test]
    fn text_cm_scales_bbox() {
        // cm で 2 倍拡大、Tm 原点 (10, 20)、fontSize=10 → device font 20, advance 10
        let contents = b"2 0 0 2 0 0 cm BT /F1 10 Tf 1 0 0 1 10 20 Tm (A) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        // origin device = (20, 40), advance device = 10 (0.5*10*2)
        assert!(approx(g[0].left, 20.0), "left={}", g[0].left);
        assert!(approx(g[0].right, 30.0), "right={}", g[0].right);
        // fontSize_dev = hypot(trm[2],trm[3]) = 20
        // top = 200 - (40 + 0.88*20) = 200 - 57.6 = 142.4
        assert!(approx(g[0].top, 142.4), "top={}", g[0].top);
        assert!(approx(g[0].bottom, 200.0 - (40.0 - 0.12 * 20.0)));
    }

    /// 制御文字スキップと空白グリフ出力
    #[test]
    fn text_control_skip_space_emit() {
        // \n \r \t は出力しない、スペースは出す
        // PDF literal string: (A\n \tB) — lexer がどう扱うか
        // バイト列で直接: A, 0x0a, space, 0x09, B
        let mut contents = b"BT /F1 10 Tf 1 0 0 1 0 50 Tm (".to_vec();
        contents.extend_from_slice(b"A");
        contents.push(b'\n');
        contents.push(b' ');
        contents.push(b'\t');
        contents.extend_from_slice(b"B) Tj ET");
        let g = glyphs_of(&contents, default_view());
        let chars: Vec<char> = g.iter().map(|x| x.ch).collect();
        assert_eq!(chars, vec!['A', ' ', 'B'], "got {chars:?}");
        // 制御文字分も字送りは進むので B の位置は A + \n + space + \t の後
        // 4 advances of 5 after A before B? glyphs: A, \n(skip), space, \t(skip), B
        // advances: A, \n, space, \t, then B at 4*5=20
        assert!(approx(g[2].left, 20.0), "B left={}", g[2].left);
    }

    /// ascent/descent 由来の top/bottom（スタブ値）
    #[test]
    fn text_ascent_descent_bbox() {
        let contents = b"BT /F1 100 Tf 1 0 0 1 0 0 Tm (X) Tj ET";
        let g = glyphs_of(contents, default_view());
        assert_eq!(g.len(), 1);
        // baseline 0, fs=100, asc=0.88 desc=-0.12
        // top = 200 - 88 = 112, bottom = 200 - (-12) = 212
        assert!(approx(g[0].top, 112.0));
        assert!(approx(g[0].bottom, 212.0));
        // 高さ 100
        assert!(approx(g[0].bottom - g[0].top, 100.0));
    }

    /// view 原点が非ゼロのときの座標変換
    #[test]
    fn text_view_origin_nonzero() {
        // view = [10, 20, 210, 220], 原点左上 PDF (10, 220)
        // glyph at Tm (30, 40), fs=10
        let view = [10.0, 20.0, 210.0, 220.0];
        let contents = b"BT /F1 10 Tf 1 0 0 1 30 40 Tm (Z) Tj ET";
        let g = glyphs_of(contents, view);
        assert_eq!(g.len(), 1);
        // left = 30-10 = 20, right = 35-10 = 25
        assert!(approx(g[0].left, 20.0));
        assert!(approx(g[0].right, 25.0));
        // top = 220 - (40 + 8.8) = 171.2
        // bottom = 220 - (40 - 1.2) = 181.2
        assert!(approx(g[0].top, 171.2), "top={}", g[0].top);
        assert!(approx(g[0].bottom, 181.2), "bottom={}", g[0].bottom);
    }
}
