//! 純Rustリーダー

mod bcmap;
mod cmap;
mod content;
mod crypto;
mod data;
mod file;
mod filters;
mod font;
mod lexer;
mod object;
mod pages;
mod parser;
mod xref;

use crate::error::Result;
use crate::extract::{ExtractOptions, PagePart, extract_from_parts};
use crate::model::Document;

use content::{ContentCtx, extract_parts};
use pages::{collect_pages, view_height, view_width};
use xref::XRef;

/// 純Rustリーダーによるバイト列からの表抽出
pub fn extract_from_bytes_rs(
    bytes: &[u8],
    password: Option<&str>,
    options: &ExtractOptions,
) -> Result<Document> {
    let pages = read_pages_rs(bytes, password)?;
    Ok(extract_from_parts(pages, options))
}

/// PDFバイト列からページ単位の parts を構築する
pub fn read_pages_rs(bytes: &[u8], password: Option<&str>) -> Result<Vec<PagePart>> {
    // 空パスワード → 引数パスワードは CipherFactory 内で試す
    let xref = XRef::parse_with_password(bytes, password)?;
    let page_infos = collect_pages(bytes, &xref)?;
    // xref の内部キャッシュはスレッド共有できないためスレッドごとに複製する
    Ok(crate::par::map_ordered(
        page_infos,
        || xref.clone(),
        |xref, _, p| build_page_part(p, bytes, xref),
    ))
}

/// ページ1枚分の parts を構築する
fn build_page_part(p: pages::PageInfo, bytes: &[u8], xref: &XRef) -> PagePart {
    let (glyphs, edges, graphics, norm_rotate) =
        extract_parts(&p.contents, &p.resources, p.view, ContentCtx {
            data: bytes,
            xref: Some(xref),
        }, p.rotate);
    let (mut width, mut height) = (view_width(p.view), view_height(p.view));
    if p.rotate == 90 || p.rotate == 270 {
        std::mem::swap(&mut width, &mut height);
    }
    if norm_rotate == 90 || norm_rotate == 270 {
        std::mem::swap(&mut width, &mut height);
    }
    PagePart {
        width,
        height,
        glyphs,
        edges,
        norm_rotate,
        graphics,
    }
}
