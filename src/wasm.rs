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

/// options_json から読み取り層の設定を取り出す
///
/// 検出系の項目は無視される。None・空文字は既定値
#[cfg(feature = "reader")]
fn reader_settings_from_json(
    options_json: Option<String>,
) -> Result<crate::extract::ReaderSettings, JsError> {
    match options_json {
        Some(s) if !s.is_empty() => {
            let options: ExtractOptions =
                serde_json::from_str(&s).map_err(|e| JsError::new(&e.to_string()))?;
            Ok(options.reader)
        }
        _ => Ok(crate::extract::ReaderSettings::default()),
    }
}

/// PDF バイト列から座標付き文字を抽出し TextDoc の JSON を返す
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_text_from_pdf(
    bytes: &[u8],
    password: Option<String>,
    options_json: Option<String>,
) -> Result<String, JsError> {
    let reader = reader_settings_from_json(options_json)?;
    let doc = crate::reader::extract_text_from_bytes_with(bytes, password.as_deref(), &reader)
        .map_err(|e| JsError::new(&e.to_string()))?;
    serde_json::to_string(&doc).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から TextItem 互換ビューを抽出しページごとの items の JSON を返す
///
/// bidi は既定オフ。有効化は Rust の `build_text_items_with` か文書入口の options を使う
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_text_items_from_pdf(
    bytes: &[u8],
    password: Option<String>,
    options_json: Option<String>,
) -> Result<String, JsError> {
    let reader = reader_settings_from_json(options_json)?;
    let doc = crate::reader::extract_text_from_bytes_with(bytes, password.as_deref(), &reader)
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
#[cfg(feature = "reader")]
#[wasm_bindgen]
pub fn extract_text_lines_from_pdf(
    bytes: &[u8],
    password: Option<String>,
    options_json: Option<String>,
) -> Result<String, JsError> {
    let reader = reader_settings_from_json(options_json)?;
    let doc = crate::reader::extract_text_from_bytes_with(bytes, password.as_deref(), &reader)
        .map_err(|e| JsError::new(&e.to_string()))?;
    let pages: Vec<Vec<crate::text::TextLine>> = doc
        .pages
        .into_iter()
        .map(|pg| crate::text::build_text_lines(&pg))
        .collect();
    serde_json::to_string(&pages).map_err(|e| JsError::new(&e.to_string()))
}

/// PDF バイト列から文書モデルを抽出し DocDoc の JSON を返す
#[cfg(feature = "document")]
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

/// PDF バイト列から Markdown 文字列を返す
#[cfg(feature = "document")]
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
    crate::document::extract_markdown_from_bytes(bytes, password.as_deref(), &options)
        .map_err(|e| JsError::new(&e.to_string()))
}

