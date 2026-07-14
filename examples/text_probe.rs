//! 座標付き文字を JSON で出力する

use pdfni::{
    build_text_items_with, build_text_lines_with, document_to_markdown,
    extract_document_from_bytes, extract_text_from_bytes, ExtractOptions,
};
use std::io::{BufRead, IsTerminal};

fn usage_and_exit() -> ! {
    eprintln!(
        "usage: text_probe [--password-stdin] <input.pdf> [lines|items|doc|md|mdnoesc|dochf|mdhf] [output.json]"
    );
    std::process::exit(2);
}

// stdin の 1 行目をパスワードとして読む。末尾の \n / \r\n は 1 回だけ除去
fn read_password_from_stdin() -> String {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!("--password-stdin was given but stdin is a TTY; pipe or redirect the password");
        std::process::exit(2);
    }
    let mut line = String::new();
    if let Err(e) = stdin.lock().read_line(&mut line) {
        eprintln!("failed to read password from stdin: {e}");
        std::process::exit(1);
    }
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    line
}

fn main() {
    let mut password_stdin = false;
    let mut positional: Vec<String> = Vec::new();
    for a in std::env::args().skip(1) {
        if a == "--password-stdin" {
            password_stdin = true;
        } else {
            positional.push(a);
        }
    }

    let mut it = positional.into_iter();
    let input = it.next().unwrap_or_else(|| usage_and_exit());

    // 2 個目は既知モード名なら mode、それ以外は出力パスとして解釈する互換挙動
    let (mode, out_path) = match it.next() {
        Some(a) => match a.as_str() {
            "items" => (Mode::Items, it.next()),
            "lines" => (Mode::Lines, it.next()),
            "doc" => (Mode::Doc { detect_hf: false }, it.next()),
            "md" => (
                Mode::Md {
                    detect_hf: false,
                    escape_markdown: true,
                },
                it.next(),
            ),
            "mdnoesc" => (
                Mode::Md {
                    detect_hf: false,
                    escape_markdown: false,
                },
                it.next(),
            ),
            "dochf" => (Mode::Doc { detect_hf: true }, it.next()),
            "mdhf" => (
                Mode::Md {
                    detect_hf: true,
                    escape_markdown: true,
                },
                it.next(),
            ),
            _ => (Mode::Chars, Some(a)),
        },
        None => (Mode::Chars, None),
    };

    if it.next().is_some() {
        usage_and_exit();
    }

    let password = if password_stdin {
        Some(read_password_from_stdin())
    } else {
        None
    };

    let bytes = match std::fs::read(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("failed to read {input}: {e}");
            std::process::exit(1);
        }
    };

    // doc / md / mdnoesc / dochf / mdhf は文書モデル入口、それ以外は座標付き文字入口
    if matches!(mode, Mode::Doc { .. } | Mode::Md { .. }) {
        let detect_hf = match mode {
            Mode::Doc { detect_hf } | Mode::Md { detect_hf, .. } => detect_hf,
            _ => false,
        };
        let escape_markdown = match mode {
            Mode::Md {
                escape_markdown, ..
            } => escape_markdown,
            _ => true,
        };
        let bidi = std::env::var_os("PDFNI_BIDI").is_some();
        let options = ExtractOptions {
            detect_header_footer: detect_hf,
            escape_markdown,
            bidi,
            ..ExtractOptions::default()
        };
        let doc = match extract_document_from_bytes(&bytes, password.as_deref(), &options) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("extract failed: {e}");
                std::process::exit(1);
            }
        };

        let output = match mode {
            Mode::Doc { .. } => match serde_json::to_string_pretty(&doc) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("serialize failed: {e}");
                    std::process::exit(1);
                }
            },
            Mode::Md {
                escape_markdown, ..
            } => document_to_markdown(&doc, escape_markdown),
            _ => unreachable!(),
        };

        if let Some(path) = out_path {
            if let Err(e) = std::fs::write(&path, &output) {
                eprintln!("failed to write {path}: {e}");
                std::process::exit(1);
            }
            eprintln!("=> {path}");
        } else {
            println!("{output}");
        }

        match mode {
            Mode::Doc { .. } | Mode::Md { .. } => {
                for (i, pg) in doc.pages.iter().enumerate() {
                    eprintln!(
                        "  page {i} {:.0}x{:.0} fonts={} blocks={}",
                        pg.width,
                        pg.height,
                        pg.fonts.len(),
                        pg.blocks.len()
                    );
                }
            }
            _ => unreachable!(),
        }
        return;
    }

    let doc = match extract_text_from_bytes(&bytes, password.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("extract failed: {e}");
            std::process::exit(1);
        }
    };

    let bidi = std::env::var_os("PDFNI_BIDI").is_some();

    // lines は JSON と件数表示で使い回す
    let lines_pages = match mode {
        Mode::Lines => Some(
            doc.pages
                .iter()
                .map(|pg| build_text_lines_with(pg, bidi))
                .collect::<Vec<_>>(),
        ),
        _ => None,
    };

    let json = match mode {
        Mode::Items => {
            let pages: Vec<_> = doc
                .pages
                .iter()
                .map(|pg| build_text_items_with(pg, bidi))
                .collect();
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
        Mode::Doc { .. } | Mode::Md { .. } => unreachable!(),
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
                let n = build_text_items_with(pg, bidi).len();
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
        Mode::Doc { .. } | Mode::Md { .. } => unreachable!(),
    }
}

enum Mode {
    Chars,
    Items,
    Lines,
    Doc { detect_hf: bool },
    Md {
        detect_hf: bool,
        escape_markdown: bool,
    },
}
