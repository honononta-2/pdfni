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
use crate::text::{TextChar, TextDoc, TextFont, TextPage};

use content::{ContentCtx, extract_parts, extract_parts_and_text, extract_text};
use pages::{collect_pages, decode_contents, view_height, view_width};
use xref::XRef;

/// 純Rustリーダーによるバイト列からの表抽出
pub fn extract_from_bytes(
    bytes: &[u8],
    password: Option<&str>,
    options: &ExtractOptions,
) -> Result<Document> {
    let xref = XRef::parse_with_limit(bytes, password, options.reader.max_decoded_bytes, options.reader.max_cmap_entries)?;
    let page_infos = collect_pages(bytes, &xref)?;
    let pages = crate::par::map_ordered(
        page_infos,
        || &xref,
        |xref, _, p| build_page_part(p, bytes, xref),
    )
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    let mut doc = extract_from_parts(pages, options);
    doc.warnings = xref.take_warnings();
    Ok(doc)
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
    let xref = XRef::parse_with_limit(bytes, password, reader.max_decoded_bytes, reader.max_cmap_entries)?;
    let page_infos = collect_pages(bytes, &xref)?;
    // XRef は Sync なので全スレッドで共有する
    let pages = crate::par::map_ordered(
        page_infos,
        || &xref,
        |xref, _, p| build_text_page(p, bytes, xref),
    )
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    Ok(TextDoc {
        pages,
        warnings: xref.take_warnings(),
    })
}

/// PDFバイト列からページ単位の parts を構築する
pub fn read_pages(bytes: &[u8], password: Option<&str>) -> Result<Vec<PagePart>> {
    read_pages_with(bytes, password, &ReaderSettings::default())
}

/// 読み取り層の設定付きの parts 構築
pub fn read_pages_with(
    bytes: &[u8],
    password: Option<&str>,
    reader: &ReaderSettings,
) -> Result<Vec<PagePart>> {
    // 空パスワード → 引数パスワードは CipherFactory 内で試す
    let xref = XRef::parse_with_limit(bytes, password, reader.max_decoded_bytes, reader.max_cmap_entries)?;
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

/// PDF バイト列からページ単位の parts と座標付き文字を 1 パスで構築する
///
/// 3 番目の戻り値は文書処理中に蓄積した警告
pub(crate) fn read_pages_and_text(
    bytes: &[u8],
    password: Option<&str>,
    reader: &ReaderSettings,
) -> Result<(Vec<PagePart>, Vec<TextPage>, Vec<String>)> {
    let xref = XRef::parse_with_limit(bytes, password, reader.max_decoded_bytes, reader.max_cmap_entries)?;
    let page_infos = collect_pages(bytes, &xref)?;
    let results: Vec<(PagePart, TextPage)> = crate::par::map_ordered(
        page_infos,
        || &xref,
        |xref, _, p| build_page_part_and_text(p, bytes, xref),
    )
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    let warnings = xref.take_warnings();
    let (parts, texts) = results.into_iter().unzip();
    Ok((parts, texts, warnings))
}

/// ページ1枚分の parts と座標付き文字を 1 パスで構築する
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

/// リーダーの全情報をバイト列から1パスで抽出する
pub fn read_pages_full(bytes: &[u8], password: Option<&str>) -> Result<ReaderDump> {
    read_pages_full_with(bytes, password, &ReaderSettings::default())
}

/// 読み取り層の設定付きの全情報抽出
pub fn read_pages_full_with(
    bytes: &[u8],
    password: Option<&str>,
    reader: &ReaderSettings,
) -> Result<ReaderDump> {
    let (parts, texts, warnings) = read_pages_and_text(bytes, password, reader)?;
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
