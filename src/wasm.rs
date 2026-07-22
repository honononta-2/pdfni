//! wasm から JS へ公開する入口

use crate::extract::{ExtractOptions, PagePart, extract_from_parts};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

/// JS から受け取る入力
#[derive(Deserialize)]
struct Input {
    pages: Vec<PagePart>,
    #[serde(default)]
    options: ExtractOptions,
}

/// 文字と線分の JSON から表抽出し Document の JSON を返す
#[wasm_bindgen]
pub fn extract_tables(input_json: &str) -> Result<String, JsError> {
    let input: Input =
        serde_json::from_str(input_json).map_err(|e| JsError::new(&e.to_string()))?;
    let doc = extract_from_parts(input.pages, &input.options);
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から純Rustリーダーで表抽出し Document の JSON を返す
///
/// options_json の pages・region キーで対象ページ・矩形を絞れる
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_tables_from_pdf(
    bytes: &[u8],
    options_json: Option<String>,
    password: Option<String>,
) -> Result<String, JsError> {
    let options = extract_options_from_json(&options_json)?;
    let (query, query_warnings) = query_from_json(&options_json)?;
    let mut doc =
        crate::reader::extract_from_bytes_with_query(bytes, password.as_deref(), &options, &query)
            .map_err(|e| JsError::new(&e.to_string()))?;
    doc.warnings.extend(query_warnings);
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// options_json から検出・読み取りの設定を取り出す
///
/// None・空文字は既定値
#[cfg(feature = "reader")]
fn extract_options_from_json(options_json: &Option<String>) -> Result<ExtractOptions, JsError> {
    match options_json {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).map_err(|e| JsError::new(&e.to_string()))
        }
        _ => Ok(ExtractOptions::default()),
    }
}

/// options_json から読み取り層の設定を取り出す
///
/// 検出系の項目は無視される
#[cfg(feature = "reader")]
fn reader_settings_from_json(
    options_json: &Option<String>,
) -> Result<crate::extract::ReaderSettings, JsError> {
    Ok(extract_options_from_json(options_json)?.reader)
}

/// options_json の絞り込みキー
#[cfg(feature = "reader")]
#[derive(Deserialize, Default)]
#[serde(default)]
struct QueryJson {
    pages: Option<String>,
    region: Option<RegionJson>,
}

/// region の LTRB / LTWH 両形式を受ける
#[cfg(feature = "reader")]
#[derive(Deserialize, Default)]
#[serde(default)]
struct RegionJson {
    left: Option<f64>,
    top: Option<f64>,
    right: Option<f64>,
    bottom: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
}

/// options_json から絞り込み条件と警告を取り出す
#[cfg(feature = "reader")]
fn query_from_json(
    options_json: &Option<String>,
) -> Result<(crate::query::ExtractQuery, Vec<String>), JsError> {
    let raw: QueryJson = match options_json {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(s).map_err(|e| JsError::new(&e.to_string()))?
        }
        _ => QueryJson::default(),
    };
    let pages = match raw.pages {
        Some(s) => Some(
            s.parse::<crate::query::PageSelection>()
                .map_err(|e| JsError::new(&e.to_string()))?,
        ),
        None => None,
    };
    let mut warnings = Vec::new();
    let region = match raw.region {
        Some(r) => Some(resolve_region(&r, &mut warnings)?),
        None => None,
    };
    Ok((crate::query::ExtractQuery { pages, region }, warnings))
}

/// LTRB 優先の矩形解決
#[cfg(feature = "reader")]
fn resolve_region(
    r: &RegionJson,
    warnings: &mut Vec<String>,
) -> Result<crate::query::Region, JsError> {
    let (Some(left), Some(top)) = (r.left, r.top) else {
        return Err(JsError::new("region requires left and top"));
    };
    if let (Some(right), Some(bottom)) = (r.right, r.bottom) {
        if r.width.is_some() || r.height.is_some() {
            warnings.push(
                "region: both right/bottom and width/height given; width/height ignored"
                    .to_string(),
            );
        }
        return Ok(crate::query::Region::from_ltrb(left, top, right, bottom));
    }
    if let (Some(width), Some(height)) = (r.width, r.height) {
        if r.right.is_some() || r.bottom.is_some() {
            warnings.push(
                "region: incomplete right/bottom ignored; using width/height".to_string(),
            );
        }
        return Ok(crate::query::Region::from_ltwh(left, top, width, height));
    }
    Err(JsError::new(
        "region requires right/bottom or width/height",
    ))
}

/// PDF バイト列から座標付き文字を抽出し TextDoc の JSON を返す
///
/// options_json の pages・region キーで対象ページ・矩形を絞れる
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_text_from_pdf(
    bytes: &[u8],
    password: Option<String>,
    options_json: Option<String>,
) -> Result<String, JsError> {
    let reader = reader_settings_from_json(&options_json)?;
    let (query, query_warnings) = query_from_json(&options_json)?;
    let mut doc = crate::reader::extract_text_from_bytes_with_query(
        bytes,
        password.as_deref(),
        &reader,
        &query,
    )
    .map_err(|e| JsError::new(&e.to_string()))?;
    doc.warnings.extend(query_warnings);
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から TextItem 互換ビューを抽出しページごとの items の JSON を返す
///
/// bidi は既定オフ。有効化は Rust の `build_text_items_with` か文書入口の options を使う
/// options_json の pages・region キーで対象ページ・矩形を絞れる
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_text_items_from_pdf(
    bytes: &[u8],
    password: Option<String>,
    options_json: Option<String>,
) -> Result<String, JsError> {
    let reader = reader_settings_from_json(&options_json)?;
    let (query, _) = query_from_json(&options_json)?;
    let doc = crate::reader::extract_text_from_bytes_with_query(
        bytes,
        password.as_deref(),
        &reader,
        &query,
    )
    .map_err(|e| JsError::new(&e.to_string()))?;
    let pages: Vec<Vec<crate::text::TextItem>> = doc
        .pages
        .iter()
        .map(crate::text::build_text_items)
        .collect();
    serde_json::to_string(&pages).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から行・単語ビューを抽出しページごとの lines の JSON を返す
///
/// bidi は既定オフ。有効化は Rust の `build_text_lines_with` か文書入口の options を使う
/// options_json の pages・region キーで対象ページ・矩形を絞れる
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_text_lines_from_pdf(
    bytes: &[u8],
    password: Option<String>,
    options_json: Option<String>,
) -> Result<String, JsError> {
    let reader = reader_settings_from_json(&options_json)?;
    let (query, _) = query_from_json(&options_json)?;
    let doc = crate::reader::extract_text_from_bytes_with_query(
        bytes,
        password.as_deref(),
        &reader,
        &query,
    )
    .map_err(|e| JsError::new(&e.to_string()))?;
    let pages: Vec<Vec<crate::text::TextLine>> = doc
        .pages
        .into_iter()
        .map(|pg| crate::text::build_text_lines(&pg))
        .collect();
    serde_json::to_string(&pages).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から文書モデルを抽出し DocDoc の JSON を返す
///
/// options_json の pages・region キーで対象ページ・矩形を絞れる
#[cfg(feature = "document")]
#[wasm_bindgen]
pub fn extract_document_from_pdf(
    bytes: &[u8],
    options_json: Option<String>,
    password: Option<String>,
) -> Result<String, JsError> {
    let options = extract_options_from_json(&options_json)?;
    let (query, query_warnings) = query_from_json(&options_json)?;
    let mut doc = crate::document::extract_document_from_bytes_with_query(
        bytes,
        password.as_deref(),
        &options,
        &query,
    )
    .map_err(|e| JsError::new(&e.to_string()))?;
    doc.warnings.extend(query_warnings);
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から Markdown 文字列を返す
///
/// options_json の pages・region キーで対象ページ・矩形を絞れる
#[cfg(feature = "document")]
#[wasm_bindgen]
pub fn extract_markdown_from_pdf(
    bytes: &[u8],
    options_json: Option<String>,
    password: Option<String>,
) -> Result<String, JsError> {
    let options = extract_options_from_json(&options_json)?;
    let (query, _) = query_from_json(&options_json)?;
    crate::document::extract_markdown_from_bytes_with_query(
        bytes,
        password.as_deref(),
        &options,
        &query,
    )
    .map_err(|e| JsError::new(&e.to_string()))
}

