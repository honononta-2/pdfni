//! 座標付き文字を JSON で出力する

use reticella::{build_text_items, build_text_lines, extract_text_from_bytes};

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| {
        eprintln!("usage: text_probe <input.pdf> [lines|items] [output.json] [password]");
        std::process::exit(2);
    });

    let mut mode = Mode::Chars;
    let mut out_path = None;
    let mut password = None;

    if let Some(a) = args.next() {
        match a.as_str() {
            "items" => {
                mode = Mode::Items;
                out_path = args.next();
                password = args.next();
            }
            "lines" => {
                mode = Mode::Lines;
                out_path = args.next();
                password = args.next();
            }
            _ => {
                out_path = Some(a);
                password = args.next();
            }
        }
    }

    let bytes = match std::fs::read(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read {input}: {e}");
            std::process::exit(1);
        }
    };

    let doc = match extract_text_from_bytes(&bytes, password.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("extract failed: {e}");
            std::process::exit(1);
        }
    };

    // lines は JSON と件数表示で使い回す
    let lines_pages = match mode {
        Mode::Lines => Some(doc.pages.iter().map(build_text_lines).collect::<Vec<_>>()),
        _ => None,
    };

    let json = match mode {
        Mode::Items => {
            let pages: Vec<_> = doc.pages.iter().map(build_text_items).collect();
            match serde_json::to_string_pretty(&pages) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("serialize failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Mode::Lines => match serde_json::to_string_pretty(lines_pages.as_ref().unwrap()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("serialize failed: {e}");
                std::process::exit(1);
            }
        },
        Mode::Chars => match serde_json::to_string_pretty(&doc) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("serialize failed: {e}");
                std::process::exit(1);
            }
        },
    };

    if let Some(path) = out_path {
        if let Err(e) = std::fs::write(&path, &json) {
            eprintln!("failed to write {path}: {e}");
            std::process::exit(1);
        }
        eprintln!("=> {path}");
    } else {
        println!("{json}");
    }

    match mode {
        Mode::Items => {
            for (i, pg) in doc.pages.iter().enumerate() {
                let n = build_text_items(pg).len();
                eprintln!(
                    "  page {i} {:.0}x{:.0} fonts={} items={}",
                    pg.width,
                    pg.height,
                    pg.fonts.len(),
                    n
                );
            }
        }
        Mode::Lines => {
            let pages = lines_pages.as_ref().unwrap();
            for (i, (pg, lines)) in doc.pages.iter().zip(pages.iter()).enumerate() {
                eprintln!(
                    "  page {i} {:.0}x{:.0} fonts={} lines={}",
                    pg.width,
                    pg.height,
                    pg.fonts.len(),
                    lines.len()
                );
            }
        }
        Mode::Chars => {
            for (i, pg) in doc.pages.iter().enumerate() {
                eprintln!(
                    "  page {i} {:.0}x{:.0} fonts={} chars={}",
                    pg.width,
                    pg.height,
                    pg.fonts.len(),
                    pg.chars.len()
                );
            }
        }
    }
}

enum Mode {
    Chars,
    Items,
    Lines,
}
