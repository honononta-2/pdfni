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

use serde::Serialize;

use crate::error::Result;
use crate::extract::{
    EdgePart, ExtractOptions, GlyphPart, GraphicPart, PagePart, ReaderSettings, extract_from_parts,
};
use crate::model::Document;
use crate::query::{ExtractQuery, PageSelection, retain_parts_in_region};
use crate::text::{TextChar, TextDoc, TextFont, TextPage, retain_chars_in_region};

use content::{ContentCtx, extract_parts, extract_parts_and_text, extract_text};
use pages::{collect_pages, decode_contents, view_height, view_width};
use xref::XRef;

/// 純Rustリーダーによるバイト列からの表抽出
pub fn extract_from_bytes(
    bytes: &[u8],
    password: Option<&str>,
    options: &ExtractOptions,
) -> Result<Document> {
    extract_from_bytes_with_query(bytes, password, options, &ExtractQuery::default())
}

/// ページ・矩形で絞った表抽出
pub fn extract_from_bytes_with_query(
    bytes: &[u8],
    password: Option<&str>,
    options: &ExtractOptions,
    query: &ExtractQuery,
) -> Result<Document> {
    let xref = XRef::parse_with_limit(bytes, password, options.reader.max_decoded_bytes, options.reader.max_cmap_entries)?;
    let page_infos = select_pages(collect_pages(bytes, &xref)?, query.pages.as_ref());
    let mut pages = crate::par::map_ordered(
        page_infos,
        || &xref,
        |xref, _, p| build_page_part(p, bytes, xref),
    )
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    if let Some(region) = &query.region {
        for page in &mut pages {
            retain_parts_in_region(page, region);
        }
    }
    let mut doc = extract_from_parts(pages, options);
    doc.warnings = xref.take_warnings();
    Ok(doc)
}

/// 1始まりのページ番号による間引き
fn select_pages(
    page_infos: Vec<pages::PageInfo>,
    sel: Option<&PageSelection>,
) -> Vec<pages::PageInfo> {
    match sel {
        Some(sel) => page_infos
            .into_iter()
            .enumerate()
            .filter(|(i, _)| sel.contains((i + 1) as u32))
            .map(|(_, p)| p)
            .collect(),
        None => page_infos,
    }
}

/// 純Rustリーダーによるバイト列からの座標付き文字抽出
pub fn extract_text_from_bytes(bytes: &[u8], password: Option<&str>) -> Result<TextDoc> {
    extract_text_from_bytes_with(bytes, password, &ReaderSettings::default())
}

/// 読み取り層の設定付きの座標付き文字抽出
pub fn extract_text_from_bytes_with(
    bytes: &[u8],
    password: Option<&str>,
    reader: &ReaderSettings,
) -> Result<TextDoc> {
    extract_text_from_bytes_with_query(bytes, password, reader, &ExtractQuery::default())
}

/// ページ・矩形で絞った座標付き文字抽出
pub fn extract_text_from_bytes_with_query(
    bytes: &[u8],
    password: Option<&str>,
    reader: &ReaderSettings,
    query: &ExtractQuery,
) -> Result<TextDoc> {
    let xref = XRef::parse_with_limit(bytes, password, reader.max_decoded_bytes, reader.max_cmap_entries)?;
    let page_infos = select_pages(collect_pages(bytes, &xref)?, query.pages.as_ref());
    // XRef は Sync のため全スレッド共有
    let mut pages = crate::par::map_ordered(
        page_infos,
        || &xref,
        |xref, _, p| build_text_page(p, bytes, xref),
    )
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    if let Some(region) = &query.region {
        for page in &mut pages {
            retain_chars_in_region(page, region);
        }
    }
    Ok(TextDoc {
        pages,
        warnings: xref.take_warnings(),
    })
}

/// PDF バイト列からのページ単位 parts 構築
pub fn read_pages(bytes: &[u8], password: Option<&str>) -> Result<Vec<PagePart>> {
    read_pages_with(bytes, password, &ReaderSettings::default())
}

/// 読み取り層の設定付きの parts 構築
pub fn read_pages_with(
    bytes: &[u8],
    password: Option<&str>,
    reader: &ReaderSettings,
) -> Result<Vec<PagePart>> {
    // 空パスワード → 引数パスワードの順の試行は CipherFactory 内
    let xref = XRef::parse_with_limit(bytes, password, reader.max_decoded_bytes, reader.max_cmap_entries)?;
    let page_infos = collect_pages(bytes, &xref)?;
    // XRef は Sync のため全スレッド共有
    crate::par::map_ordered(
        page_infos,
        || &xref,
        |xref, _, p| build_page_part(p, bytes, xref),
    )
    .into_iter()
    .collect()
}

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

/// parts と座標付き文字の 1 パス構築
///
/// 3 番目の戻り値は文書処理中に蓄積した警告
pub(crate) fn read_pages_and_text(
    bytes: &[u8],
    password: Option<&str>,
    reader: &ReaderSettings,
    query: &ExtractQuery,
) -> Result<(Vec<PagePart>, Vec<TextPage>, Vec<String>)> {
    let xref = XRef::parse_with_limit(bytes, password, reader.max_decoded_bytes, reader.max_cmap_entries)?;
    let page_infos = select_pages(collect_pages(bytes, &xref)?, query.pages.as_ref());
    let results: Vec<(PagePart, TextPage)> = crate::par::map_ordered(
        page_infos,
        || &xref,
        |xref, _, p| build_page_part_and_text(p, bytes, xref),
    )
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    let warnings = xref.take_warnings();
    let (mut parts, mut texts): (Vec<PagePart>, Vec<TextPage>) = results.into_iter().unzip();
    if let Some(region) = &query.region {
        for part in &mut parts {
            retain_parts_in_region(part, region);
        }
        for text in &mut texts {
            retain_chars_in_region(text, region);
        }
    }
    Ok((parts, texts, warnings))
}

fn build_page_part_and_text(
    p: pages::PageInfo,
    bytes: &[u8],
    xref: &XRef,
) -> Result<(PagePart, TextPage)> {
    let contents = decode_contents(bytes, xref, &p.contents)?;
    let (parts, text) = extract_parts_and_text(
        &contents,
        &p.resources,
        p.view,
        ContentCtx {
            data: bytes,
            xref: Some(xref),
        },
        p.rotate,
    );
    let (mut width, mut height) = (view_width(p.view), view_height(p.view));
    if p.rotate == 90 || p.rotate == 270 {
        std::mem::swap(&mut width, &mut height);
    }
    if parts.norm_rotate == 90 || parts.norm_rotate == 270 {
        std::mem::swap(&mut width, &mut height);
    }
    let part = PagePart {
        width,
        height,
        glyphs: parts.glyphs,
        edges: parts.edges,
        norm_rotate: parts.norm_rotate,
        graphics: parts.graphics,
    };
    let tp = TextPage {
        width: text.width,
        height: text.height,
        fonts: text.fonts,
        chars: text.chars,
    };
    Ok((part, tp))
}

/// リーダーが1回のパスで出せる全情報のダンプ
#[derive(Debug, Clone, Serialize)]
pub struct ReaderDump {
    pub pages: Vec<ReaderDumpPage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// ダンプ1ページ分の情報
#[derive(Debug, Clone, Serialize)]
pub struct ReaderDumpPage {
    pub width: f64,
    pub height: f64,
    pub norm_rotate: i32,
    pub text: TextView,
    pub parts: PartsView,
}

/// テキスト系ビュー
#[derive(Debug, Clone, Serialize)]
pub struct TextView {
    pub fonts: Vec<TextFont>,
    pub chars: Vec<TextChar>,
}

/// parts 系ビュー
#[derive(Debug, Clone, Serialize)]
pub struct PartsView {
    pub glyphs: Vec<GlyphPart>,
    pub edges: Vec<EdgePart>,
    pub graphics: Vec<GraphicPart>,
}

/// 全情報の 1 パス抽出
pub fn read_pages_full(bytes: &[u8], password: Option<&str>) -> Result<ReaderDump> {
    read_pages_full_with(bytes, password, &ReaderSettings::default())
}

/// 読み取り層の設定付きの全情報抽出
pub fn read_pages_full_with(
    bytes: &[u8],
    password: Option<&str>,
    reader: &ReaderSettings,
) -> Result<ReaderDump> {
    let (parts, texts, warnings) =
        read_pages_and_text(bytes, password, reader, &ExtractQuery::default())?;
    let pages = parts
        .into_iter()
        .zip(texts)
        .map(|(part, text)| ReaderDumpPage {
            width: part.width,
            height: part.height,
            norm_rotate: part.norm_rotate,
            text: TextView {
                fonts: text.fonts,
                chars: text.chars,
            },
            parts: PartsView {
                glyphs: part.glyphs,
                edges: part.edges,
                graphics: part.graphics,
            },
        })
        .collect();
    Ok(ReaderDump { pages, warnings })
}
