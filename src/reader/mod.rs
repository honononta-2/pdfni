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
use crate::text::{TextDoc, TextPage};

use content::{ContentCtx, extract_parts, extract_text};
use pages::{collect_pages, decode_contents, view_height, view_width};
use xref::XRef;

/// 純Rustリーダーによるバイト列からの表抽出
pub fn extract_from_bytes(
    bytes: &[u8],
    password: Option<&str>,
    options: &ExtractOptions,
) -> Result<Document> {
    let pages = read_pages(bytes, password)?;
    Ok(extract_from_parts(pages, options))
}

/// 純Rustリーダーによるバイト列からの座標付き文字抽出
pub fn extract_text_from_bytes(bytes: &[u8], password: Option<&str>) -> Result<TextDoc> {
    let xref = XRef::parse_with_password(bytes, password)?;
    let page_infos = collect_pages(bytes, &xref)?;
    // XRef は Sync なので全スレッドで共有する
    let pages = crate::par::map_ordered(
        page_infos,
        || &xref,
        |xref, _, p| build_text_page(p, bytes, xref),
    )
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    Ok(TextDoc { pages })
}

/// PDFバイト列からページ単位の parts を構築する
pub fn read_pages(bytes: &[u8], password: Option<&str>) -> Result<Vec<PagePart>> {
    // 空パスワード → 引数パスワードは CipherFactory 内で試す
    let xref = XRef::parse_with_password(bytes, password)?;
    let page_infos = collect_pages(bytes, &xref)?;
    // XRef は Sync なので全スレッドで共有する
    crate::par::map_ordered(
        page_infos,
        || &xref,
        |xref, _, p| build_page_part(p, bytes, xref),
    )
    .into_iter()
    .collect()
}

/// ページ1枚分の parts を構築する
fn build_page_part(p: pages::PageInfo, bytes: &[u8], xref: &XRef) -> Result<PagePart> {
    let contents = decode_contents(bytes, xref, &p.contents)?;
    let parts = extract_parts(&contents, &p.resources, p.view, ContentCtx {
        data: bytes,
        xref: Some(xref),
    }, p.rotate);
    let (mut width, mut height) = (view_width(p.view), view_height(p.view));
    if p.rotate == 90 || p.rotate == 270 {
        std::mem::swap(&mut width, &mut height);
    }
    if parts.norm_rotate == 90 || parts.norm_rotate == 270 {
        std::mem::swap(&mut width, &mut height);
    }
    Ok(PagePart {
        width,
        height,
        glyphs: parts.glyphs,
        edges: parts.edges,
        norm_rotate: parts.norm_rotate,
        graphics: parts.graphics,
    })
}

/// ページ1枚分の座標付き文字を構築する
fn build_text_page(p: pages::PageInfo, bytes: &[u8], xref: &XRef) -> Result<TextPage> {
    let contents = decode_contents(bytes, xref, &p.contents)?;
    let text = extract_text(
        &contents,
        &p.resources,
        p.view,
        ContentCtx {
            data: bytes,
            xref: Some(xref),
        },
        p.rotate,
    );
    Ok(TextPage {
        width: text.width,
        height: text.height,
        fonts: text.fonts,
        chars: text.chars,
    })
}
