//! pdfium リーダー層
//! 座標は CropBox 原点基準の top-down に揃える

use crate::error::{Error, Result};
use crate::model::{BBox, Edge, Glyph, Orientation};
use pdfium_render::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn bind_pdfium(dll_dir: Option<&Path>) -> Result<Pdfium> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(d) = dll_dir {
        candidates.push(d.to_path_buf());
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        candidates.push(parent.to_path_buf());
    }
    candidates.push(std::path::PathBuf::from("."));

    let mut last_err: Option<pdfium_render::prelude::PdfiumError> = None;
    for dir in &candidates {
        match Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(dir)) {
            Ok(b) => return Ok(Pdfium::new(b)),
            Err(e) => last_err = Some(e),
        }
    }
    let bindings = Pdfium::bind_to_system_library()
        .map_err(|e| Error::PdfiumBind(format!("system: {e:?}; last: {last_err:?}")))?;
    Ok(Pdfium::new(bindings))
}

/// JS 側で initialize_pdfium_render() 済みが前提
#[cfg(target_arch = "wasm32")]
pub(crate) fn bind_pdfium() -> Result<Pdfium> {
    let bindings =
        Pdfium::bind_to_system_library().map_err(|e| Error::PdfiumBind(format!("{e:?}")))?;
    Ok(Pdfium::new(bindings))
}

/// ページ左端と上端のユーザー空間座標
pub(crate) fn page_origin(page: &PdfPage, page_h: f64) -> (f64, f64) {
    let b = page.boundaries();
    match b.crop().or_else(|_| b.media()) {
        Ok(bb) => (bb.bounds.left().value as f64, bb.bounds.top().value as f64),
        Err(_) => (0.0, page_h),
    }
}

pub(crate) fn extract_chars(page: &PdfPage, origin: (f64, f64)) -> Result<Vec<Glyph>> {
    let (ox, oy) = origin;
    let text = page
        .text()
        .map_err(|e| Error::PageRead(format!("text: {e:?}")))?;
    let mut glyphs: Vec<Glyph> = Vec::new();
    for c in text.chars().iter() {
        let ch = match c.unicode_char() {
            Some(ch) => ch,
            None => continue,
        };
        if ch == '\n' || ch == '\r' || ch == '\t' {
            continue;
        }
        let r = match c.loose_bounds().or_else(|_| c.tight_bounds()) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let bbox = BBox {
            x0: r.left().value as f64 - ox,
            x1: r.right().value as f64 - ox,
            top: oy - r.top().value as f64,
            bottom: oy - r.bottom().value as f64,
        };
        glyphs.push(Glyph { ch, bbox });
    }
    Ok(glyphs)
}

/// 2点を結ぶ軸平行な線分を辺にする
fn axis_edge(a: (f32, f32), b: (f32, f32), origin: (f64, f64), eps: f32) -> Option<Edge> {
    let (ox, oy) = origin;
    let (px, py) = a;
    let (x, y) = b;
    if (x - px).abs() <= eps && (y - py).abs() > eps {
        Some(Edge {
            x0: x as f64 - ox,
            x1: x as f64 - ox,
            top: oy - py.max(y) as f64,
            bottom: oy - py.min(y) as f64,
            orientation: Orientation::Vertical,
        })
    } else if (y - py).abs() <= eps && (x - px).abs() > eps {
        let yy = oy - y as f64;
        Some(Edge {
            x0: px.min(x) as f64 - ox,
            x1: px.max(x) as f64 - ox,
            top: yy,
            bottom: yy,
            orientation: Orientation::Horizontal,
        })
    } else {
        None
    }
}

pub(crate) fn extract_edges(page: &PdfPage, origin: (f64, f64)) -> Vec<Edge> {
    let eps = 0.8_f32;
    let mut edges: Vec<Edge> = Vec::new();
    for obj in page.objects().iter() {
        collect_edges(&obj, &PdfMatrix::IDENTITY, origin, eps, &mut edges);
    }
    edges
}

/// パスと Form XObject からの軸平行な線分の収集
fn collect_edges(
    obj: &PdfPageObject,
    parent: &PdfMatrix,
    origin: (f64, f64),
    eps: f32,
    edges: &mut Vec<Edge>,
) {
    if let Some(form) = obj.as_x_object_form_object() {
        // 入れ子の行列の合成
        let m = form.matrix().unwrap_or(PdfMatrix::IDENTITY).multiply(*parent);
        for child in form.iter() {
            collect_edges(&child, &m, origin, eps, edges);
        }
    } else if let Some(path_obj) = obj.as_path_object() {
        // ページ座標への変換
        let m = path_obj
            .matrix()
            .unwrap_or(PdfMatrix::IDENTITY)
            .multiply(*parent);
        let apply = |x: f32, y: f32| -> (f32, f32) {
            let (tx, ty) = m.apply_to_points(PdfPoints::new(x), PdfPoints::new(y));
            (tx.value, ty.value)
        };
        // 閉じ線を合成する条件
        let filled = path_obj
            .fill_mode()
            .map(|f| f != PdfPathFillMode::None)
            .unwrap_or(false);
        let mut sub_start: Option<(f32, f32)> = None;
        let mut prev: Option<(f32, f32)> = None;
        for seg in path_obj.segments().iter() {
            let pt = apply(seg.x().value, seg.y().value);
            match seg.segment_type() {
                PdfPathSegmentType::MoveTo => {
                    // 塗りの閉じ線
                    if filled && let (Some(s), Some(p)) = (sub_start, prev) {
                        if let Some(e) = axis_edge(p, s, origin, eps) {
                            edges.push(e);
                        }
                    }
                    sub_start = Some(pt);
                    prev = Some(pt);
                }
                PdfPathSegmentType::LineTo => {
                    if let Some(p) = prev {
                        if let Some(e) = axis_edge(p, pt, origin, eps) {
                            edges.push(e);
                        }
                    }
                    prev = Some(pt);
                }
                _ => {
                    prev = Some(pt);
                }
            }
            // 明示的な閉じの閉じ線
            if seg.is_close() && let Some(s) = sub_start {
                if let Some(e) = axis_edge(pt, s, origin, eps) {
                    edges.push(e);
                }
                prev = Some(s);
            }
        }
        // 塗りの閉じ線
        if filled && let (Some(s), Some(p)) = (sub_start, prev) {
            if let Some(e) = axis_edge(p, s, origin, eps) {
                edges.push(e);
            }
        }
    }
}
