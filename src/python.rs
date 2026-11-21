//! Python 拡張モジュールの入口

use crate::error::Error;
use crate::extract::{ExtractOptions, PartsInput, ReaderSettings, extract_from_parts};
use crate::options_json::{
    OptionsError, extract_options_from_json, query_from_json, reader_settings_from_json,
};
use crate::query::ExtractQuery;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyByteArray, PyBytes, PyDict, PyMemoryView, PyString};
use serde::Serialize;
use serde_json::{Map, Value};
use std::path::PathBuf;

create_exception!(pdfni, PdfniError, PyException);
create_exception!(pdfni, BadPasswordError, PdfniError);
create_exception!(pdfni, SecurityRestrictedError, PdfniError);
create_exception!(pdfni, InvalidFormatError, PdfniError);

/// GIL 解放中も参照できる PDF 入力
enum Source<'py> {
    Bytes(Bound<'py, PyBytes>),
    File(Vec<u8>),
}

impl Source<'_> {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes.as_bytes(),
            Self::File(data) => data,
        }
    }
}

/// str と os.PathLike はファイルパス、可変のバイト列は複製
fn read_source<'py>(source: &Bound<'py, PyAny>) -> PyResult<Source<'py>> {
    if let Ok(bytes) = source.cast::<PyBytes>() {
        return Ok(Source::Bytes(bytes.clone()));
    }
    if source.is_instance_of::<PyByteArray>() || source.is_instance_of::<PyMemoryView>() {
        let copied = source.py().get_type::<PyBytes>().call1((source,))?;
        return Ok(Source::Bytes(copied.cast_into::<PyBytes>()?));
    }
    if source.is_instance_of::<PyString>() || source.hasattr("__fspath__")? {
        let path: PathBuf = source.extract()?;
        return Ok(Source::File(std::fs::read(path)?));
    }
    Err(PyTypeError::new_err(
        "source must be bytes, bytearray, memoryview, str, or os.PathLike",
    ))
}

/// キーワード引数による options の上書き
#[derive(Default)]
struct Overrides {
    pages: Option<String>,
    region: Option<(f64, f64, f64, f64)>,
    mode: Option<String>,
    detect_header_footer: Option<bool>,
}

/// キーワード引数を優先した options JSON
fn options_json(
    options: Option<&Bound<'_, PyDict>>,
    overrides: Overrides,
) -> PyResult<Option<String>> {
    let mut map = match options {
        Some(dict) => serde_json::from_str::<Map<String, Value>>(&json_dumps(dict)?)
            .map_err(|e| PyValueError::new_err(e.to_string()))?,
        None => Map::new(),
    };
    if let Some(pages) = overrides.pages {
        map.insert("pages".into(), Value::String(pages));
    }
    if let Some((left, top, right, bottom)) = overrides.region {
        map.insert(
            "region".into(),
            serde_json::json!({ "left": left, "top": top, "right": right, "bottom": bottom }),
        );
    }
    if let Some(mode) = overrides.mode {
        map.insert("mode".into(), Value::String(mode));
    }
    if let Some(detect) = overrides.detect_header_footer {
        map.insert("detect_header_footer".into(), Value::Bool(detect));
    }
    if map.is_empty() {
        return Ok(None);
    }
    Ok(Some(Value::Object(map).to_string()))
}

fn json_dumps(value: &Bound<'_, PyAny>) -> PyResult<String> {
    value
        .py()
        .import("json")?
        .call_method1("dumps", (value,))?
        .extract()
}

fn options_error(err: OptionsError) -> PyErr {
    PyValueError::new_err(err.to_string())
}

fn extract_error(err: Error) -> PyErr {
    let message = err.to_string();
    match err {
        Error::BadPassword => BadPasswordError::new_err(message),
        Error::SecurityRestricted => SecurityRestrictedError::new_err(message),
        Error::InvalidFormat => InvalidFormatError::new_err(message),
        Error::InvalidPageSelection(_) => PyValueError::new_err(message),
        Error::PdfOpen(source) => source.into(),
        _ => PdfniError::new_err(message),
    }
}

/// 検出設定と絞り込み条件と解釈時の警告
fn detect_settings(
    options_json: Option<&str>,
) -> PyResult<(ExtractOptions, ExtractQuery, Vec<String>)> {
    let options = extract_options_from_json(options_json).map_err(options_error)?;
    let (query, warnings) = query_from_json(options_json).map_err(options_error)?;
    Ok((options, query, warnings))
}

/// 読み取り設定と絞り込み条件と解釈時の警告
fn reader_settings(
    options_json: Option<&str>,
) -> PyResult<(ReaderSettings, ExtractQuery, Vec<String>)> {
    let reader = reader_settings_from_json(options_json).map_err(options_error)?;
    let (query, warnings) = query_from_json(options_json).map_err(options_error)?;
    Ok((reader, query, warnings))
}

/// GIL を解放した抽出と Python オブジェクトへの変換
fn extract_to_python<'py, T, F>(py: Python<'py>, extract: F) -> PyResult<Bound<'py, PyAny>>
where
    T: Serialize + Send,
    F: FnOnce() -> Result<T, Error> + Send,
{
    let value = py.detach(extract).map_err(extract_error)?;
    Ok(pythonize::pythonize(py, &value)?)
}

/// 入力は parts の dict、出力は Document の dict
#[pyfunction]
fn extract_tables<'py>(parts: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let input: PartsInput = serde_json::from_str(&json_dumps(parts)?)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    extract_to_python(parts.py(), move || {
        Ok(extract_from_parts(input.pages, &input.options))
    })
}

/// 出力は Document の dict
#[pyfunction]
#[pyo3(signature = (source, *, password=None, pages=None, region=None, mode=None, detect_header_footer=None, options=None))]
fn extract_tables_from_pdf<'py>(
    source: &Bound<'py, PyAny>,
    password: Option<String>,
    pages: Option<String>,
    region: Option<(f64, f64, f64, f64)>,
    mode: Option<String>,
    detect_header_footer: Option<bool>,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let overrides = Overrides {
        pages,
        region,
        mode,
        detect_header_footer,
    };
    let (options, query, warnings) =
        detect_settings(options_json(options, overrides)?.as_deref())?;
    let input = read_source(source)?;
    let data = input.as_slice();
    extract_to_python(source.py(), move || {
        let mut doc = crate::reader::extract_from_bytes_with_query(
            data,
            password.as_deref(),
            &options,
            &query,
        )?;
        doc.warnings.extend(warnings);
        Ok(doc)
    })
}

/// 出力は TextDoc の dict
#[pyfunction]
#[pyo3(signature = (source, *, password=None, pages=None, region=None, options=None))]
fn extract_text_from_pdf<'py>(
    source: &Bound<'py, PyAny>,
    password: Option<String>,
    pages: Option<String>,
    region: Option<(f64, f64, f64, f64)>,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let overrides = Overrides {
        pages,
        region,
        ..Overrides::default()
    };
    let (reader, query, warnings) =
        reader_settings(options_json(options, overrides)?.as_deref())?;
    let input = read_source(source)?;
    let data = input.as_slice();
    extract_to_python(source.py(), move || {
        let mut doc = crate::reader::extract_text_from_bytes_with_query(
            data,
            password.as_deref(),
            &reader,
            &query,
        )?;
        doc.warnings.extend(warnings);
        Ok(doc)
    })
}

/// 出力はページごとの items の list
#[pyfunction]
#[pyo3(signature = (source, *, password=None, pages=None, region=None, options=None))]
fn extract_text_items_from_pdf<'py>(
    source: &Bound<'py, PyAny>,
    password: Option<String>,
    pages: Option<String>,
    region: Option<(f64, f64, f64, f64)>,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let overrides = Overrides {
        pages,
        region,
        ..Overrides::default()
    };
    let (reader, query, _) = reader_settings(options_json(options, overrides)?.as_deref())?;
    let input = read_source(source)?;
    let data = input.as_slice();
    extract_to_python(source.py(), move || {
        let doc = crate::reader::extract_text_from_bytes_with_query(
            data,
            password.as_deref(),
            &reader,
            &query,
        )?;
        let pages: Vec<Vec<crate::text::TextItem>> = doc
            .pages
            .iter()
            .map(crate::text::build_text_items)
            .collect();
        Ok(pages)
    })
}

/// 出力はページごとの lines の list
#[pyfunction]
#[pyo3(signature = (source, *, password=None, pages=None, region=None, options=None))]
fn extract_text_lines_from_pdf<'py>(
    source: &Bound<'py, PyAny>,
    password: Option<String>,
    pages: Option<String>,
    region: Option<(f64, f64, f64, f64)>,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let overrides = Overrides {
        pages,
        region,
        ..Overrides::default()
    };
    let (reader, query, _) = reader_settings(options_json(options, overrides)?.as_deref())?;
    let input = read_source(source)?;
    let data = input.as_slice();
    extract_to_python(source.py(), move || {
        let doc = crate::reader::extract_text_from_bytes_with_query(
            data,
            password.as_deref(),
            &reader,
            &query,
        )?;
        let pages: Vec<Vec<crate::text::TextLine>> = doc
            .pages
            .into_iter()
            .map(|page| crate::text::build_text_lines(&page))
            .collect();
        Ok(pages)
    })
}

/// 出力は DocDoc の dict
#[pyfunction]
#[pyo3(signature = (source, *, password=None, pages=None, region=None, mode=None, detect_header_footer=None, options=None))]
fn extract_document_from_pdf<'py>(
    source: &Bound<'py, PyAny>,
    password: Option<String>,
    pages: Option<String>,
    region: Option<(f64, f64, f64, f64)>,
    mode: Option<String>,
    detect_header_footer: Option<bool>,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let overrides = Overrides {
        pages,
        region,
        mode,
        detect_header_footer,
    };
    let (options, query, warnings) =
        detect_settings(options_json(options, overrides)?.as_deref())?;
    let input = read_source(source)?;
    let data = input.as_slice();
    extract_to_python(source.py(), move || {
        let mut doc = crate::document::extract_document_from_bytes_with_query(
            data,
            password.as_deref(),
            &options,
            &query,
        )?;
        doc.warnings.extend(warnings);
        Ok(doc)
    })
}

/// 出力は DocContent の dict
#[pyfunction]
#[pyo3(signature = (source, *, password=None, pages=None, region=None, mode=None, detect_header_footer=None, options=None))]
fn extract_content_from_pdf<'py>(
    source: &Bound<'py, PyAny>,
    password: Option<String>,
    pages: Option<String>,
    region: Option<(f64, f64, f64, f64)>,
    mode: Option<String>,
    detect_header_footer: Option<bool>,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let overrides = Overrides {
        pages,
        region,
        mode,
        detect_header_footer,
    };
    let (options, query, warnings) =
        detect_settings(options_json(options, overrides)?.as_deref())?;
    let input = read_source(source)?;
    let data = input.as_slice();
    extract_to_python(source.py(), move || {
        let mut doc = crate::document::extract_content_from_bytes_with_query(
            data,
            password.as_deref(),
            &options,
            &query,
        )?;
        doc.warnings.extend(warnings);
        Ok(doc)
    })
}

#[pymodule]
mod pdfni {
    #[pymodule_export]
    use super::{
        BadPasswordError, InvalidFormatError, PdfniError, SecurityRestrictedError,
        extract_content_from_pdf, extract_document_from_pdf, extract_tables,
        extract_tables_from_pdf, extract_text_from_pdf, extract_text_items_from_pdf,
        extract_text_lines_from_pdf,
    };
}
