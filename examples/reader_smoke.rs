//! Smoke test: print page count, size, glyph and edge counts via reader.

use pdfni::read_pages;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: reader_smoke <input.pdf>");
        std::process::exit(2);
    });

    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read {path}: {e}");
            std::process::exit(1);
        }
    };

    let pages = match read_pages(&bytes, None) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("read failed: {e}");
            std::process::exit(1);
        }
    };

    println!("pages: {}", pages.len());
    for (i, p) in pages.iter().enumerate() {
        println!(
            "page {}: width={} height={} glyphs={} edges={}",
            i + 1,
            p.width,
            p.height,
            p.glyphs.len(),
            p.edges.len()
        );
    }
}
