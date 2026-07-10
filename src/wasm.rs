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

