//! ページツリー走査と view / Contents の解決
//!
//! 準拠: pdf.js `catalog.js` getPageDict / getAllPageDicts、`document.js` Page

use std::collections::HashSet;

use crate::error::{Error, Result};

use super::filters::decode_stream;
use super::object::{Dict, Object, Ref};
use super::parser::Resolver;
use super::xref::XRef;

/// レターサイズ MediaBox（ポイント）
const LETTER_SIZE: [f64; 4] = [0.0, 0.0, 612.0, 792.0];

/// 1ページ分の表示領域・リソース・コンテンツ
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct PageInfo {
    /// 正規化済み [x0, y0, x1, y1]（PDF 座標系のまま）
    pub view: [f64; 4],
    /// ページ回転 /Rotate（0/90/180/270へ正規化済み）
    pub rotate: i32,
    pub resources: Dict,
    pub contents: Vec<u8>,
}

/// data と xref を紐づけた Resolver
struct FetchResolver<'a> {
    xref: &'a XRef,
    data: &'a [u8],
}

impl Resolver for FetchResolver<'_> {
    fn resolve(&self, r: Ref) -> Option<Object> {
        self.xref.fetch(r, self.data).ok()
    }
}

/// trailer /Root → /Pages を DFS してページを列挙する
pub(crate) fn collect_pages(data: &[u8], xref: &XRef) -> Result<Vec<PageInfo>> {
    let root_raw = xref
        .trailer()
        .get("Root")
        .ok_or_else(|| Error::Reader("trailer missing /Root".into()))?;
    let root = xref.resolve(root_raw, data);
    let Object::Dict(catalog) = root else {
        return Err(Error::Reader("catalog is not a dictionary".into()));
    };
    let pages_raw = catalog
        .get("Pages")
        .ok_or_else(|| Error::Reader("catalog missing /Pages".into()))?;

    let mut visited: HashSet<(u32, u16)> = HashSet::new();
    if let Object::Ref(r) = pages_raw {
        visited.insert((r.num, r.generation));
    }

    let mut stack: Vec<Object> = vec![xref.resolve(pages_raw, data)];
    let mut pages: Vec<PageInfo> = Vec::new();

    while let Some(node) = stack.pop() {
        match node {
            Object::Ref(r) => {
                if !visited.insert((r.num, r.generation)) {
                    return Err(Error::Reader(format!(
                        "Pages tree contains circular reference: {} {}",
                        r.num, r.generation
                    )));
                }
                let obj = xref.fetch(r, data)?;
                stack.push(obj);
            }
            Object::Dict(dict) => {
                if is_page_leaf(&dict, xref, data) {
                    pages.push(build_page_info(data, xref, &dict)?);
                    continue;
                }

                let kids_raw = match dict.get("Kids") {
                    Some(k) => k,
                    None => {
                        // Kids 無しは is_page_leaf で葉になるはずだが安全側
                        pages.push(build_page_info(data, xref, &dict)?);
                        continue;
                    }
                };
                let kids = xref.resolve(kids_raw, data);
                match kids {
                    Object::Array(arr) => {
                        // 逆順 push で先頭 Kids から処理（pdf.js と同様）
                        for kid in arr.iter().rev() {
                            stack.push(kid.clone());
                        }
                    }
                    _ => {
                        // Kids が配列でない仕様違反: 自身を葉として扱う
                        pages.push(build_page_info(data, xref, &dict)?);
                    }
                }
            }
            Object::Null => {}
            other => {
                return Err(Error::Reader(format!(
                    "Page dictionary kid reference points to wrong type of object: {other:?}"
                )));
            }
        }
    }

    Ok(pages)
}

/// /Type が Page、または /Kids を持たないノードを葉とする
fn is_page_leaf(dict: &Dict, xref: &XRef, data: &[u8]) -> bool {
    if type_is_page(dict, xref, data) {
        return true;
    }
    !dict.has("Kids")
}

fn type_is_page(dict: &Dict, xref: &XRef, data: &[u8]) -> bool {
    match dict.get("Type") {
        Some(Object::Name(n)) => n == "Page",
        Some(Object::Ref(r)) => {
            matches!(xref.fetch(*r, data), Ok(Object::Name(n)) if n == "Page")
        }
        _ => false,
    }
}

fn build_page_info(data: &[u8], xref: &XRef, page_dict: &Dict) -> Result<PageInfo> {
    let media = resolve_media_box(data, xref, page_dict);
    let crop = resolve_crop_box(data, xref, page_dict, media);
    let view = compute_view(crop, media);
    let rotate = resolve_rotate(data, xref, page_dict);
    let resources = resolve_resources(data, xref, page_dict);
    let contents = load_contents(data, xref, page_dict)?;
    Ok(PageInfo {
        view,
        rotate,
        resources,
        contents,
    })
}

/// 継承つき /Rotate の解決。90度の倍数以外は0として扱う
fn resolve_rotate(data: &[u8], xref: &XRef, page_dict: &Dict) -> i32 {
    let mut visited: HashSet<(u32, u16)> = HashSet::new();
    let mut current: Option<Dict> = Some(page_dict.clone());
    while let Some(dict) = current {
        if let Some(obj) = dict.get("Rotate") {
            if let Object::Int(r) = xref.resolve(obj, data) {
                let r = r.rem_euclid(360);
                return if r % 90 == 0 { r as i32 } else { 0 };
            }
        }
        current = follow_parent(data, xref, &dict, &mut visited);
    }
    0
}

fn resolve_media_box(data: &[u8], xref: &XRef, page_dict: &Dict) -> [f64; 4] {
    match inherit_box(data, xref, page_dict, "MediaBox") {
        Some(b) if is_valid_rect(b) => b,
        _ => LETTER_SIZE,
    }
}

fn resolve_crop_box(
    data: &[u8],
    xref: &XRef,
    page_dict: &Dict,
    media: [f64; 4],
) -> [f64; 4] {
    match inherit_box(data, xref, page_dict, "CropBox") {
        Some(b) if is_valid_rect(b) => b,
        _ => media,
    }
}

/// CropBox ∩ MediaBox。交差が空なら MediaBox
fn compute_view(crop: [f64; 4], media: [f64; 4]) -> [f64; 4] {
    if crop == media {
        return media;
    }
    match intersect_rect(crop, media) {
        Some(b) if is_valid_rect(b) => b,
        _ => media,
    }
}

/// Parent チェーンを遡り、最初に見つかった矩形を返す（正規化済み）
fn inherit_box(
    data: &[u8],
    xref: &XRef,
    page_dict: &Dict,
    key: &str,
) -> Option<[f64; 4]> {
    let mut visited: HashSet<(u32, u16)> = HashSet::new();
    let mut current: Option<Dict> = Some(page_dict.clone());

    while let Some(dict) = current {
        if let Some(obj) = dict.get(key) {
            let resolved = xref.resolve(obj, data);
            if let Some(rect) = parse_rect(&resolved) {
                return Some(normalize_rect(rect));
            }
        }
        current = follow_parent(data, xref, &dict, &mut visited);
    }
    None
}

/// Resources を全階層から集め、子優先で浅いマージする
fn resolve_resources(data: &[u8], xref: &XRef, page_dict: &Dict) -> Dict {
    let mut dicts: Vec<Dict> = Vec::new();
    let mut visited: HashSet<(u32, u16)> = HashSet::new();
    let mut current: Option<Dict> = Some(page_dict.clone());

    while let Some(dict) = current {
        if let Some(obj) = dict.get("Resources") {
            if let Object::Dict(rd) = xref.resolve(obj, data) {
                dicts.push(rd);
            }
        }
        current = follow_parent(data, xref, &dict, &mut visited);
    }

    let refs: Vec<&Dict> = dicts.iter().collect();
    if refs.is_empty() {
        Dict::new()
    } else {
        Dict::merge(&refs)
    }
}

fn follow_parent(
    data: &[u8],
    xref: &XRef,
    dict: &Dict,
    visited: &mut HashSet<(u32, u16)>,
) -> Option<Dict> {
    match dict.get("Parent") {
        Some(Object::Ref(r)) => {
            if !visited.insert((r.num, r.generation)) {
                return None;
            }
            match xref.fetch(*r, data) {
                Ok(Object::Dict(d)) => Some(d),
                _ => None,
            }
        }
        Some(Object::Dict(d)) => Some(d.clone()),
        _ => None,
    }
}

fn load_contents(data: &[u8], xref: &XRef, page_dict: &Dict) -> Result<Vec<u8>> {
    let Some(raw) = page_dict.get("Contents") else {
        return Ok(Vec::new());
    };
    let contents = xref.resolve(raw, data);
    let resolver = FetchResolver { xref, data };

    match contents {
        Object::Stream(s) => decode_stream(data, &s, &resolver, xref.cipher()),
        Object::Array(arr) => {
            let mut out = Vec::new();
            let mut first = true;
            for item in &arr {
                let obj = xref.resolve(item, data);
                let Object::Stream(s) = obj else {
                    continue;
                };
                let decoded = decode_stream(data, &s, &resolver, xref.cipher())?;
                if !first {
                    out.push(b' ');
                }
                first = false;
                out.extend_from_slice(&decoded);
            }
            Ok(out)
        }
        _ => Ok(Vec::new()),
    }
}

fn parse_rect(obj: &Object) -> Option<[f64; 4]> {
    let Object::Array(arr) = obj else {
        return None;
    };
    if arr.len() < 4 {
        return None;
    }
    let a = as_f64(&arr[0])?;
    let b = as_f64(&arr[1])?;
    let c = as_f64(&arr[2])?;
    let d = as_f64(&arr[3])?;
    Some([a, b, c, d])
}

fn as_f64(obj: &Object) -> Option<f64> {
    match obj {
        Object::Int(n) => Some(*n as f64),
        Object::Real(n) => Some(*n),
        _ => None,
    }
}

fn normalize_rect(r: [f64; 4]) -> [f64; 4] {
    let mut out = r;
    if out[0] > out[2] {
        out.swap(0, 2);
    }
    if out[1] > out[3] {
        out.swap(1, 3);
    }
    out
}

fn is_valid_rect(r: [f64; 4]) -> bool {
    r[2] - r[0] > 0.0 && r[3] - r[1] > 0.0
}

fn intersect_rect(a: [f64; 4], b: [f64; 4]) -> Option<[f64; 4]> {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = a[2].min(b[2]);
    let y1 = a[3].min(b[3]);
    if x0 > x1 || y0 > y1 {
        None
    } else {
        Some([x0, y0, x1, y1])
    }
}

/// view の幅
#[allow(dead_code)]
pub(crate) fn view_width(view: [f64; 4]) -> f64 {
    view[2] - view[0]
}

/// view の高さ
#[allow(dead_code)]
pub(crate) fn view_height(view: [f64; 4]) -> f64 {
    view[3] - view[1]
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// オブジェクト列と xref/trailer を組み立てる簡易 PDF ビルダ
    struct PdfBuilder {
        body: Vec<u8>,
        offsets: Vec<usize>,
    }

    impl PdfBuilder {
        fn new() -> Self {
            let mut body = Vec::new();
            body.extend_from_slice(b"%PDF-1.4\n");
            // オブジェクト番号 0 は free
            Self {
                body,
                offsets: vec![0],
            }
        }

        fn add_obj(&mut self, content: &[u8]) -> u32 {
            let num = self.offsets.len() as u32;
            self.offsets.push(self.body.len());
            self.body
                .extend_from_slice(format!("{num} 0 obj\n").as_bytes());
            self.body.extend_from_slice(content);
            if !content.ends_with(b"\n") {
                self.body.push(b'\n');
            }
            self.body.extend_from_slice(b"endobj\n");
            num
        }

        /// ストリームオブジェクトを追加し (obj_num, raw_payload) を返す
        fn add_stream(&mut self, dict_extra: &str, payload: &[u8]) -> u32 {
            let num = self.offsets.len() as u32;
            self.offsets.push(self.body.len());
            let dict = format!(
                "{num} 0 obj\n<< /Length {} {} >>\nstream\n",
                payload.len(),
                dict_extra
            );
            self.body.extend_from_slice(dict.as_bytes());
            self.body.extend_from_slice(payload);
            self.body.extend_from_slice(b"\nendstream\nendobj\n");
            num
        }

        fn finish(mut self, root: u32) -> Vec<u8> {
            let xref_off = self.body.len();
            let n = self.offsets.len();
            self.body
                .extend_from_slice(format!("xref\n0 {n}\n").as_bytes());
            self.body
                .extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
            for &off in &self.offsets[1..] {
                self.body
                    .extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
            }
            self.body.extend_from_slice(
                format!("trailer\n<< /Size {n} /Root {root} 0 R >>\n").as_bytes(),
            );
            self.body.extend_from_slice(b"startxref\n");
            self.body
                .extend_from_slice(format!("{xref_off}\n").as_bytes());
            self.body.extend_from_slice(b"%%EOF\n");
            self.body
        }
    }

    /// 2階層 Pages ツリーで正しい順にページ列挙
    #[test]
    fn nested_pages_tree_order() {
        let mut b = PdfBuilder::new();
        // 1,2,3 が葉。4 が中間 Pages。5 がルート Pages
        let p_a = b.add_obj(
            b"<< /Type /Page /Parent 4 0 R /MediaBox [0 0 100 200] /Resources << >> >>",
        );
        let p_b = b.add_obj(
            b"<< /Type /Page /Parent 4 0 R /MediaBox [0 0 110 210] /Resources << >> >>",
        );
        let p_c = b.add_obj(
            b"<< /Type /Page /Parent 5 0 R /MediaBox [0 0 120 220] /Resources << >> >>",
        );
        let intermediate = b.add_obj(
            format!(
                "<< /Type /Pages /Parent 5 0 R /Kids [{p_a} 0 R {p_b} 0 R] /Count 2 >>"
            )
            .as_bytes(),
        );
        assert_eq!(intermediate, 4);
        let pages_root = b.add_obj(
            format!(
                "<< /Type /Pages /Kids [{intermediate} 0 R {p_c} 0 R] /Count 3 >>"
            )
            .as_bytes(),
        );
        assert_eq!(pages_root, 5);
        let catalog = b.add_obj(
            format!("<< /Type /Catalog /Pages {pages_root} 0 R >>").as_bytes(),
        );
        let data = b.finish(catalog);

        let xref = XRef::parse(&data).expect("parse");
        let pages = collect_pages(&data, &xref).expect("collect");
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].view, [0.0, 0.0, 100.0, 200.0]);
        assert_eq!(pages[1].view, [0.0, 0.0, 110.0, 210.0]);
        assert_eq!(pages[2].view, [0.0, 0.0, 120.0, 220.0]);
    }

    /// MediaBox / Resources の継承（親定義・子未定義）
    #[test]
    fn inherit_mediabox_and_resources() {
        let mut b = PdfBuilder::new();
        let page = b.add_obj(b"<< /Type /Page /Parent 2 0 R >>");
        // 親に MediaBox と Resources（/Font キー）
        let pages_root = b.add_obj(
            format!(
                "<< /Type /Pages /Kids [{page} 0 R] /Count 1 \
                 /MediaBox [0 0 200 300] \
                 /Resources << /Font << /F1 3 0 R >> >> >>"
            )
            .as_bytes(),
        );
        assert_eq!(pages_root, 2);
        let _font = b.add_obj(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        let catalog = b.add_obj(
            format!("<< /Type /Catalog /Pages {pages_root} 0 R >>").as_bytes(),
        );
        let data = b.finish(catalog);

        let xref = XRef::parse(&data).expect("parse");
        let pages = collect_pages(&data, &xref).expect("collect");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].view, [0.0, 0.0, 200.0, 300.0]);
        assert!(
            pages[0].resources.has("Font"),
            "Resources should inherit /Font from parent"
        );
    }

    /// /Rotate の継承（親定義・子未定義）と正規化
    #[test]
    fn inherit_rotate_from_parent() {
        let mut b = PdfBuilder::new();
        let page = b.add_obj(b"<< /Type /Page /Parent 2 0 R >>");
        let pages_root = b.add_obj(
            format!(
                "<< /Type /Pages /Kids [{page} 0 R] /Count 1 \
                 /MediaBox [0 0 200 300] /Rotate 450 >>"
            )
            .as_bytes(),
        );
        let catalog = b.add_obj(
            format!("<< /Type /Catalog /Pages {pages_root} 0 R >>").as_bytes(),
        );
        let data = b.finish(catalog);
        let xref = XRef::parse(&data).expect("parse");
        let pages = collect_pages(&data, &xref).expect("collect");
        assert_eq!(pages[0].rotate, 90);
    }

    /// CropBox が MediaBox からはみ出すときの交差
    #[test]
    fn cropbox_media_intersection() {
        let mut b = PdfBuilder::new();
        let page = b.add_obj(
            b"<< /Type /Page /Parent 2 0 R \
               /MediaBox [0 0 612 792] \
               /CropBox [-50 -50 100 150] \
               /Resources << >> >>",
        );
        let pages = b.add_obj(
            format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>").as_bytes(),
        );
        let catalog = b.add_obj(
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        let data = b.finish(catalog);

        let xref = XRef::parse(&data).expect("parse");
        let pages = collect_pages(&data, &xref).expect("collect");
        assert_eq!(pages.len(), 1);
        // CropBox ∩ MediaBox = [0, 0, 100, 150]
        assert_eq!(pages[0].view, [0.0, 0.0, 100.0, 150.0]);
    }

    /// MediaBox 欠落時はレターサイズ
    #[test]
    fn missing_mediabox_letter_size() {
        let mut b = PdfBuilder::new();
        let page = b.add_obj(
            b"<< /Type /Page /Parent 2 0 R /Resources << >> >>",
        );
        let pages = b.add_obj(
            format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>").as_bytes(),
        );
        let catalog = b.add_obj(
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        let data = b.finish(catalog);

        let xref = XRef::parse(&data).expect("parse");
        let pages = collect_pages(&data, &xref).expect("collect");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].view, LETTER_SIZE);
        assert_eq!(view_width(pages[0].view), 612.0);
        assert_eq!(view_height(pages[0].view), 792.0);
    }

    /// /Contents 配列は空白1バイトを挟んで連結
    #[test]
    fn contents_array_joined_with_space() {
        let mut b = PdfBuilder::new();
        let s1 = b.add_stream("", b"BT");
        let s2 = b.add_stream("", b"ET");
        let page = b.add_obj(
            format!(
                "<< /Type /Page /Parent 4 0 R /MediaBox [0 0 100 100] \
                 /Resources << >> /Contents [{s1} 0 R {s2} 0 R] >>"
            )
            .as_bytes(),
        );
        let pages = b.add_obj(
            format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>").as_bytes(),
        );
        let catalog = b.add_obj(
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        let data = b.finish(catalog);

        let xref = XRef::parse(&data).expect("parse");
        let pages = collect_pages(&data, &xref).expect("collect");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].contents, b"BT ET");
    }

    /// Kids に Page 辞書がインラインで入っている救済
    #[test]
    fn inline_page_dict_in_kids() {
        let mut b = PdfBuilder::new();
        // Pages の Kids に参照ではなく辞書を直接埋め込む
        let pages = b.add_obj(
            b"<< /Type /Pages /Count 1 /Kids [ \
               << /Type /Page /Parent 1 0 R /MediaBox [0 0 50 60] /Resources << >> >> \
               ] >>",
        );
        assert_eq!(pages, 1);
        let catalog = b.add_obj(b"<< /Type /Catalog /Pages 1 0 R >>");
        let data = b.finish(catalog);

        let xref = XRef::parse(&data).expect("parse");
        let pages = collect_pages(&data, &xref).expect("collect");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].view, [0.0, 0.0, 50.0, 60.0]);
    }

    /// 矩形正規化（逆転した座標）
    #[test]
    fn normalize_swapped_mediabox() {
        let mut b = PdfBuilder::new();
        let page = b.add_obj(
            b"<< /Type /Page /Parent 2 0 R /MediaBox [200 300 0 0] /Resources << >> >>",
        );
        let pages = b.add_obj(
            format!("<< /Type /Pages /Kids [{page} 0 R] /Count 1 >>").as_bytes(),
        );
        let catalog = b.add_obj(
            format!("<< /Type /Catalog /Pages {pages} 0 R >>").as_bytes(),
        );
        let data = b.finish(catalog);

        let xref = XRef::parse(&data).expect("parse");
        let pages = collect_pages(&data, &xref).expect("collect");
        assert_eq!(pages[0].view, [0.0, 0.0, 200.0, 300.0]);
    }
}
