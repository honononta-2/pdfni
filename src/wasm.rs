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
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_tables_from_pdf(
    bytes: &[u8],
    options_json: Option<String>,
    password: Option<String>,
) -> Result<String, JsError> {
    let options: ExtractOptions = match options_json {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(&s).map_err(|e| JsError::new(&e.to_string()))?
        }
        _ => ExtractOptions::default(),
    };
    let doc = crate::reader::extract_from_bytes(bytes, password.as_deref(), &options)
        .map_err(|e| JsError::new(&e.to_string()))?;
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から座標付き文字を抽出し TextDoc の JSON を返す
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_text_from_pdf(
    bytes: &[u8],
    password: Option<String>,
) -> Result<String, JsError> {
    let doc = crate::reader::extract_text_from_bytes(bytes, password.as_deref())
        .map_err(|e| JsError::new(&e.to_string()))?;
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から TextItem 互換ビューを抽出しページごとの items の JSON を返す
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_text_items_from_pdf(
    bytes: &[u8],
    password: Option<String>,
) -> Result<String, JsError> {
    let doc = crate::reader::extract_text_from_bytes(bytes, password.as_deref())
        .map_err(|e| JsError::new(&e.to_string()))?;
    let pages: Vec<Vec<crate::text::TextItem>> = doc
        .pages
        .iter()
        .map(crate::text::build_text_items)
        .collect();
    serde_json::to_string(&pages).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から行・単語ビューを抽出しページごとの lines の JSON を返す
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_text_lines_from_pdf(
    bytes: &[u8],
    password: Option<String>,
) -> Result<String, JsError> {
    let doc = crate::reader::extract_text_from_bytes(bytes, password.as_deref())
        .map_err(|e| JsError::new(&e.to_string()))?;
    let pages: Vec<Vec<crate::text::TextLine>> = doc
        .pages
        .into_iter()
        .map(|pg| crate::text::build_text_lines(&pg))
        .collect();
    serde_json::to_string(&pages).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から文書モデルを抽出し DocDoc の JSON を返す
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_document_from_pdf(
    bytes: &[u8],
    options_json: Option<String>,
    password: Option<String>,
) -> Result<String, JsError> {
    let options: ExtractOptions = match options_json {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(&s).map_err(|e| JsError::new(&e.to_string()))?
        }
        _ => ExtractOptions::default(),
    };
    let doc =
        crate::document::extract_document_from_bytes(bytes, password.as_deref(), &options)
            .map_err(|e| JsError::new(&e.to_string()))?;
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から文書モデルを抽出し Markdown 文字列を返す
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_markdown_from_pdf(
    bytes: &[u8],
    options_json: Option<String>,
    password: Option<String>,
) -> Result<String, JsError> {
    let options: ExtractOptions = match options_json {
        Some(s) if !s.is_empty() => {
            serde_json::from_str(&s).map_err(|e| JsError::new(&e.to_string()))?
        }
        _ => ExtractOptions::default(),
    };
    let doc =
        crate::document::extract_document_from_bytes(bytes, password.as_deref(), &options)
            .map_err(|e| JsError::new(&e.to_string()))?;
    Ok(crate::document::document_to_markdown(
        &doc,
        options.escape_markdown,
    ))
}

