//! PDFから表を抽出しJSONを出すCLI
//! 使い方: reticella <input.pdf> [output.json] [password]

use anyhow::Result;
use std::env;

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let input = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("使い方: reticella <input.pdf> [output.json] [password]");
            std::process::exit(2);
        }
    };

    let output = args.next();
    let password = args.next();

    let doc = reticella::extract(&input, password.as_deref())?;
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
