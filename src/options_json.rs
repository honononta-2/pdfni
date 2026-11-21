//! 入口共通の options JSON 解釈

use crate::error::Error;
use crate::extract::{ExtractOptions, ReaderSettings};
use crate::query::{ExtractQuery, PageSelection, Region};
use serde::Deserialize;

/// options JSON の解釈エラー
#[derive(Debug)]
pub(crate) enum OptionsError {
    Json(serde_json::Error),
    Pages(Error),
    Region(&'static str),
}

impl std::fmt::Display for OptionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => e.fmt(f),
            Self::Pages(e) => e.fmt(f),
            Self::Region(message) => f.write_str(message),
        }
    }
}

/// options JSON の絞り込みキー
#[derive(Deserialize, Default)]
#[serde(default)]
struct QueryJson {
    pages: Option<String>,
    region: Option<RegionJson>,
}

/// region は LTRB / LTWH 両形式に対応
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

/// None・空文字は既定値
pub(crate) fn extract_options_from_json(
    options_json: Option<&str>,
) -> Result<ExtractOptions, OptionsError> {
    match options_json {
        Some(s) if !s.is_empty() => serde_json::from_str(s).map_err(OptionsError::Json),
        _ => Ok(ExtractOptions::default()),
    }
}

/// 検出系の項目は無視
pub(crate) fn reader_settings_from_json(
    options_json: Option<&str>,
) -> Result<ReaderSettings, OptionsError> {
    Ok(extract_options_from_json(options_json)?.reader)
}

/// 絞り込み条件と解釈時の警告
///
/// pages は前後の空白を除き空文字は指定なし
pub(crate) fn query_from_json(
    options_json: Option<&str>,
) -> Result<(ExtractQuery, Vec<String>), OptionsError> {
    let raw: QueryJson = match options_json {
        Some(s) if !s.is_empty() => serde_json::from_str(s).map_err(OptionsError::Json)?,
        _ => QueryJson::default(),
    };
    let pages = match raw.pages.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => Some(
            s.parse::<PageSelection>()
                .map_err(OptionsError::Pages)?,
        ),
        _ => None,
    };
    let mut warnings = Vec::new();
    let region = match raw.region {
        Some(r) => Some(resolve_region(&r, &mut warnings)?),
        None => None,
    };
    Ok((ExtractQuery { pages, region }, warnings))
}

/// LTRB 優先の矩形解決
fn resolve_region(r: &RegionJson, warnings: &mut Vec<String>) -> Result<Region, OptionsError> {
    let (Some(left), Some(top)) = (r.left, r.top) else {
        return Err(OptionsError::Region("region requires left and top"));
    };
    if let (Some(right), Some(bottom)) = (r.right, r.bottom) {
        if r.width.is_some() || r.height.is_some() {
            warnings.push(
                "region: both right/bottom and width/height given; width/height ignored"
                    .to_string(),
            );
        }
        return Ok(Region::from_ltrb(left, top, right, bottom));
    }
    if let (Some(width), Some(height)) = (r.width, r.height) {
        if r.right.is_some() || r.bottom.is_some() {
            warnings.push(
                "region: incomplete right/bottom ignored; using width/height".to_string(),
            );
        }
        return Ok(Region::from_ltwh(left, top, width, height));
    }
    Err(OptionsError::Region(
        "region requires right/bottom or width/height",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::DetectMode;

    fn query(json: &str) -> (ExtractQuery, Vec<String>) {
        query_from_json(Some(json)).unwrap()
    }

    fn query_error(json: &str) -> OptionsError {
        query_from_json(Some(json)).unwrap_err()
    }

    #[test]
    fn options_absent_or_empty_is_default() {
        assert!(matches!(
            extract_options_from_json(None).unwrap().mode,
            DetectMode::Auto
        ));
        assert!(matches!(
            extract_options_from_json(Some("")).unwrap().mode,
            DetectMode::Auto
        ));
        let (q, warnings) = query_from_json(None).unwrap();
        assert!(q.pages.is_none() && q.region.is_none() && warnings.is_empty());
    }

    #[test]
    fn options_fields_are_read() {
        let options = extract_options_from_json(Some(r#"{"mode":"ruled"}"#)).unwrap();
        assert!(matches!(options.mode, DetectMode::Ruled));
    }

    #[test]
    fn options_invalid_json_is_json_error() {
        assert!(matches!(
            extract_options_from_json(Some("{")),
            Err(OptionsError::Json(_))
        ));
        assert!(matches!(query_error("{"), OptionsError::Json(_)));
    }

    #[test]
    fn reader_settings_take_reader_section_only() {
        let reader = reader_settings_from_json(Some(
            r#"{"mode":"ruled","reader":{"max_decoded_bytes":1024}}"#,
        ))
        .unwrap();
        assert_eq!(reader.max_decoded_bytes, 1024);
    }

    #[test]
    fn pages_are_trimmed() {
        let (q, _) = query(r#"{"pages":" 1,3-5 "}"#);
        let pages = q.pages.unwrap();
        assert!(pages.contains(1) && pages.contains(4) && !pages.contains(2));
    }

    #[test]
    fn pages_blank_is_unset() {
        assert!(query(r#"{"pages":""}"#).0.pages.is_none());
        assert!(query(r#"{"pages":"   "}"#).0.pages.is_none());
    }

    #[test]
    fn pages_invalid_is_page_selection_error() {
        assert!(matches!(
            query_error(r#"{"pages":"0"}"#),
            OptionsError::Pages(Error::InvalidPageSelection(_))
        ));
    }

    #[test]
    fn region_ltrb() {
        let (q, warnings) = query(r#"{"region":{"left":1,"top":2,"right":3,"bottom":4}}"#);
        assert_eq!(q.region, Some(Region::from_ltrb(1.0, 2.0, 3.0, 4.0)));
        assert!(warnings.is_empty());
    }

    #[test]
    fn region_ltwh() {
        let (q, warnings) = query(r#"{"region":{"left":1,"top":2,"width":3,"height":4}}"#);
        assert_eq!(q.region, Some(Region::from_ltrb(1.0, 2.0, 4.0, 6.0)));
        assert!(warnings.is_empty());
    }

    #[test]
    fn region_ltrb_wins_over_size_with_warning() {
        let (q, warnings) = query(
            r#"{"region":{"left":1,"top":2,"right":3,"bottom":4,"width":9,"height":9}}"#,
        );
        assert_eq!(q.region, Some(Region::from_ltrb(1.0, 2.0, 3.0, 4.0)));
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn region_incomplete_ltrb_falls_back_to_size_with_warning() {
        let (q, warnings) =
            query(r#"{"region":{"left":1,"top":2,"right":3,"width":3,"height":4}}"#);
        assert_eq!(q.region, Some(Region::from_ltrb(1.0, 2.0, 4.0, 6.0)));
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn region_without_origin_is_error() {
        let err = query_error(r#"{"region":{"right":3,"bottom":4}}"#);
        assert_eq!(err.to_string(), "region requires left and top");
    }

    #[test]
    fn region_without_extent_is_error() {
        let err = query_error(r#"{"region":{"left":1,"top":2}}"#);
        assert_eq!(err.to_string(), "region requires right/bottom or width/height");
    }
}
