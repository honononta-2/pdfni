//! ネイティブ向けの C ABI 入口

use crate::error::Error;
use crate::extract::{ExtractOptions, ReaderSettings};
use crate::options_json::{self, OptionsError};
use crate::query::ExtractQuery;
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::slice;

const STATUS_OK: i32 = 0;
const STATUS_BAD_PASSWORD: i32 = 1;
const STATUS_INVALID_PAGE_SELECTION: i32 = 2;
const STATUS_SECURITY_RESTRICTED: i32 = 3;
const STATUS_INVALID_FORMAT: i32 = 4;
const STATUS_EXTRACT_FAILED: i32 = 5;

fn into_c_string_ptr(text: String) -> *mut c_char {
    let sanitized = if text.as_bytes().contains(&0) {
        text.replace('\0', "")
    } else {
        text
    };
    CString::new(sanitized)
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

unsafe fn read_optional_c_str(ptr: *const c_char) -> Result<Option<String>, String> {
    if ptr.is_null() {
        return Ok(None);
    }

    let text = unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|e| e.to_string())?;
    if text.is_empty() {
        return Ok(None);
    }

    Ok(Some(text.to_owned()))
}

fn map_error_status(err: &Error) -> i32 {
    match err {
        Error::BadPassword => STATUS_BAD_PASSWORD,
        Error::InvalidPageSelection(_) => STATUS_INVALID_PAGE_SELECTION,
        Error::SecurityRestricted => STATUS_SECURITY_RESTRICTED,
        Error::InvalidFormat => STATUS_INVALID_FORMAT,
        _ => STATUS_EXTRACT_FAILED,
    }
}

fn map_options_error(err: OptionsError) -> (i32, String) {
    let status = match &err {
        OptionsError::Pages(e) => map_error_status(e),
        _ => STATUS_EXTRACT_FAILED,
    };
    (status, err.to_string())
}

fn extract_options_from_json(options_json: Option<&str>) -> Result<ExtractOptions, (i32, String)> {
    options_json::extract_options_from_json(options_json).map_err(map_options_error)
}

fn reader_settings_from_json(options_json: Option<&str>) -> Result<ReaderSettings, (i32, String)> {
    options_json::reader_settings_from_json(options_json).map_err(map_options_error)
}

fn query_from_json(
    options_json: Option<&str>,
) -> Result<(ExtractQuery, Vec<String>), (i32, String)> {
    options_json::query_from_json(options_json).map_err(map_options_error)
}

/// FFI 境界の引数検証と panic 捕捉
unsafe fn run_ffi_call<F>(
    pdf_ptr: *const u8,
    pdf_len: usize,
    options_json_ptr: *const c_char,
    password_ptr: *const c_char,
    out_json: *mut *mut c_char,
    out_error: *mut *mut c_char,
    action: F,
) -> i32
where
    F: FnOnce(&[u8], Option<&str>, Option<&str>) -> Result<String, (i32, String)>,
{
    if !out_json.is_null() {
        unsafe { *out_json = std::ptr::null_mut() };
    }
    if !out_error.is_null() {
        unsafe { *out_error = std::ptr::null_mut() };
    }

    let result = catch_unwind(AssertUnwindSafe(|| {
        if pdf_ptr.is_null() || pdf_len == 0 || out_json.is_null() {
            return Err((STATUS_EXTRACT_FAILED, "invalid arguments".to_owned()));
        }

        let bytes = unsafe { slice::from_raw_parts(pdf_ptr, pdf_len) };
        let options_json = unsafe { read_optional_c_str(options_json_ptr) }
            .map_err(|e| (STATUS_EXTRACT_FAILED, e))?;
        let password = unsafe { read_optional_c_str(password_ptr) }
            .map_err(|e| (STATUS_BAD_PASSWORD, e))?;

        action(bytes, options_json.as_deref(), password.as_deref())
    }));

    match result {
        Ok(Ok(json)) => {
            unsafe { *out_json = into_c_string_ptr(json) };
            STATUS_OK
        }
        Ok(Err((code, message))) => {
            if !out_error.is_null() {
                unsafe { *out_error = into_c_string_ptr(message) };
            }
            code
        }
        Err(_) => {
            if !out_error.is_null() {
                unsafe { *out_error = into_c_string_ptr("unexpected panic in pdfni".to_owned()) };
            }
            STATUS_EXTRACT_FAILED
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pdfni_extract_tables_json(
    pdf_ptr: *const u8,
    pdf_len: usize,
    options_json_ptr: *const c_char,
    password_ptr: *const c_char,
    out_json: *mut *mut c_char,
    out_error: *mut *mut c_char,
) -> i32 {
    unsafe {
        run_ffi_call(
            pdf_ptr,
            pdf_len,
            options_json_ptr,
            password_ptr,
            out_json,
            out_error,
            |bytes, options_json, password| {
                let options = extract_options_from_json(options_json)?;
                let (query, query_warnings) = query_from_json(options_json)?;
                let mut doc =
                    crate::reader::extract_from_bytes_with_query(bytes, password, &options, &query)
                        .map_err(|e| (map_error_status(&e), e.to_string()))?;
                doc.warnings.extend(query_warnings);
                serde_json::to_string(&doc).map_err(|e| (STATUS_EXTRACT_FAILED, e.to_string()))
            },
        )
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pdfni_extract_text_lines_json(
    pdf_ptr: *const u8,
    pdf_len: usize,
    options_json_ptr: *const c_char,
    password_ptr: *const c_char,
    out_json: *mut *mut c_char,
    out_error: *mut *mut c_char,
) -> i32 {
    unsafe {
        run_ffi_call(
            pdf_ptr,
            pdf_len,
            options_json_ptr,
            password_ptr,
            out_json,
            out_error,
            |bytes, options_json, password| {
                let reader = reader_settings_from_json(options_json)?;
                let (query, _) = query_from_json(options_json)?;
                let doc = crate::reader::extract_text_from_bytes_with_query(
                    bytes, password, &reader, &query,
                )
                .map_err(|e| (map_error_status(&e), e.to_string()))?;
                let pages: Vec<Vec<crate::text::TextLine>> = doc
                    .pages
                    .into_iter()
                    .map(|page| crate::text::build_text_lines(&page))
                    .collect();
                serde_json::to_string(&pages).map_err(|e| (STATUS_EXTRACT_FAILED, e.to_string()))
            },
        )
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pdfni_extract_text_json(
    pdf_ptr: *const u8,
    pdf_len: usize,
    options_json_ptr: *const c_char,
    password_ptr: *const c_char,
    out_json: *mut *mut c_char,
    out_error: *mut *mut c_char,
) -> i32 {
    unsafe {
        run_ffi_call(
            pdf_ptr,
            pdf_len,
            options_json_ptr,
            password_ptr,
            out_json,
            out_error,
            |bytes, options_json, password| {
                let reader = reader_settings_from_json(options_json)?;
                let (query, query_warnings) = query_from_json(options_json)?;
                let mut doc = crate::reader::extract_text_from_bytes_with_query(
                    bytes, password, &reader, &query,
                )
                .map_err(|e| (map_error_status(&e), e.to_string()))?;
                doc.warnings.extend(query_warnings);
                serde_json::to_string(&doc).map_err(|e| (STATUS_EXTRACT_FAILED, e.to_string()))
            },
        )
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pdfni_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        let _ = unsafe { CString::from_raw(ptr) };
    }
}
