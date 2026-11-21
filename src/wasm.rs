//! wasm から JS へ公開する入口

use crate::extract::{PartsInput, extract_from_parts};
use wasm_bindgen::prelude::*;

/// 入力は文字と線分の JSON、出力は Document の JSON
#[wasm_bindgen]
pub fn extract_tables(input_json: &str) -> Result<String, JsError> {
    let input: PartsInput =
        serde_json::from_str(input_json).map_err(|e| JsError::new(&e.to_string()))?;
    let doc = extract_from_parts(input.pages, &input.options);
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// 出力は Document の JSON
///
/// options_json の pages・region キーで対象ページ・矩形を絞り込み可能
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

#[cfg(feature = "reader")]
fn extract_options_from_json(
    options_json: &Option<String>,
) -> Result<crate::extract::ExtractOptions, JsError> {
    crate::options_json::extract_options_from_json(options_json.as_deref())
        .map_err(|e| JsError::new(&e.to_string()))
}

#[cfg(feature = "reader")]
fn reader_settings_from_json(
    options_json: &Option<String>,
) -> Result<crate::extract::ReaderSettings, JsError> {
    crate::options_json::reader_settings_from_json(options_json.as_deref())
        .map_err(|e| JsError::new(&e.to_string()))
}

#[cfg(feature = "reader")]
fn query_from_json(
    options_json: &Option<String>,
) -> Result<(crate::query::ExtractQuery, Vec<String>), JsError> {
    crate::options_json::query_from_json(options_json.as_deref())
        .map_err(|e| JsError::new(&e.to_string()))
}

/// 出力は TextDoc の JSON
///
/// options_json の pages・region キーで対象ページ・矩形を絞り込み可能
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

/// 出力はページごとの items（TextItem 互換ビュー）の JSON
///
/// bidi は既定オフ。有効化は Rust の `build_text_items_with` か文書入口の options
/// options_json の pages・region キーで対象ページ・矩形を絞り込み可能
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

/// 出力はページごとの lines（行・単語ビュー）の JSON
///
/// bidi は既定オフ。有効化は Rust の `build_text_lines_with` か文書入口の options
/// options_json の pages・region キーで対象ページ・矩形を絞り込み可能
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

/// 出力は DocDoc の JSON
///
/// options_json の pages・region キーで対象ページ・矩形を絞り込み可能
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

/// 出力は DocContent の JSON
///
/// options_json の pages・region キーで対象ページ・矩形を絞り込み可能
#[cfg(feature = "document")]
#[wasm_bindgen]
pub fn extract_content_from_pdf(
    bytes: &[u8],
    options_json: Option<String>,
    password: Option<String>,
) -> Result<String, JsError> {
    let options = extract_options_from_json(&options_json)?;
    let (query, query_warnings) = query_from_json(&options_json)?;
    let mut doc = crate::document::extract_content_from_bytes_with_query(
        bytes,
        password.as_deref(),
        &options,
        &query,
    )
    .map_err(|e| JsError::new(&e.to_string()))?;
    doc.warnings.extend(query_warnings);
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// 出力は Markdown 文字列
///
/// options_json の pages・region キーで対象ページ・矩形を絞り込み可能
#[cfg(feature = "markdown")]
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

