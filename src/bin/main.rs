//! PDFから表を抽出しJSONを出すCLI
//! 使い方: reticella [--mode auto|ruled|borderless] <input.pdf> [output.json] [password]

use anyhow::Result;
use reticella::{DetectMode, ExtractOptions};
use std::env;

fn usage() -> ! {
    eprintln!("使い方: reticella [--mode auto|ruled|borderless] <input.pdf> [output.json] [password]");
    std::process::exit(2);
}

fn main() -> Result<()> {
    let mut mode = DetectMode::Auto;
    let mut positional = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--mode" {
            mode = match args.next().as_deref() {
                Some("auto") => DetectMode::Auto,
                Some("ruled") => DetectMode::Ruled,
                Some("borderless") => DetectMode::Borderless,
                _ => usage(),
            };
        } else {
            positional.push(a);
        }
    }
    let mut positional = positional.into_iter();
    let input = positional.next().unwrap_or_else(|| usage());
    let output = positional.next();
    let password = positional.next();

    let options = ExtractOptions {
        mode,
        ..ExtractOptions::default()
    };
    let bytes = std::fs::read(&input)?;
    let mut doc = reticella::extract_from_bytes(&bytes, password.as_deref(), &options)?;
    doc.source = input.clone();
    let json = serde_json::to_string_pretty(&doc)?;

    match output {
        Some(out) => {
            std::fs::write(&out, json)?;
            eprintln!("=> {out}");
        }
        None => println!("{json}"),
    }
    Ok(())
}
