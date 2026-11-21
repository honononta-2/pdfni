# pdfni

[![Crates.io](https://img.shields.io/crates/v/pdfni.svg)](https://crates.io/crates/pdfni)
[![docs.rs](https://docs.rs/pdfni/badge.svg)](https://docs.rs/pdfni)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://github.com/honononta-2/pdfni/blob/master/LICENSE)

A fast, rule-based PDF table extraction library for digital-native PDFs, written in pure Rust.

> `pdfni` is currently in `0.x`; expect breaking changes before `1.0`.

## Features

- Rule-based, deterministic table extraction for digital-native PDFs
- Automatic detection of ruled and borderless tables, with manual mode selection
- Built-in pure-Rust PDF reader adapted from [Mozilla pdf.js](https://github.com/mozilla/pdf.js) with no external runtime dependencies
- Parallel page processing on native targets and GIL release in Python
- Rust crate, CLI, Python package, C ABI shared library, and WebAssembly support
- Reading-order document blocks in the same pass, plus opt-in Markdown export via the `markdown` feature
- Encrypted PDFs using RC4, AES-128, or AES-256, page rotation, bundled CJK CMaps, and page or region filtering

## Overview

`pdfni` is a pure-Rust library built to extract tables from digital-native PDFs with high accuracy and speed. Rather than relying on OCR or machine learning models, it works directly from character positions and vector lines inside the PDF using deterministic, rule-based algorithms, so the same document always yields the same output.

Because extraction quality depends on accurate glyph coordinates and ruling lines, `pdfni` includes its own PDF reader adapted from [Mozilla pdf.js](https://github.com/mozilla/pdf.js). Everything needed to parse encrypted PDFs, rotated pages, and CJK fonts is built in, with no external runtime dependencies. The same core engine runs natively in Rust, as a CLI, from Python, and in the browser via WebAssembly.

Both ruled and borderless tables are detected automatically. Since page layout and reading order are already resolved during extraction, `pdfni` can also output structured text blocks and tables in reading order, or export Markdown when needed.

## Benchmarks

End-to-end full pipeline (table regions are not given), scored with each benchmark's official evaluation tool. Scripts and setup live under [`bench/`](https://github.com/honononta-2/pdfni/tree/master/bench).

| Benchmark | Task | Precision | Recall | F1 |
| --- | --- | --- | --- | --- |
| [ICDAR 2013 original](http://www.tamirhassan.com/html/dataset.html) (eu + us) | Region detection (char-level) | 0.9382 | 0.9316 | 0.9349 |
| [ICDAR 2013 original](http://www.tamirhassan.com/html/dataset.html) (eu + us) | Structure (cell adjacency) | 0.9522 | 0.9414 | 0.9468 |
| [ICDAR 2013 corrected](https://huggingface.co/datasets/bsmock/ICDAR-2013-Table-Competition-Corrected) (eu + us) | Region detection (char-level) | 0.9382 | 0.9316 | 0.9349 |
| [ICDAR 2013 corrected](https://huggingface.co/datasets/bsmock/ICDAR-2013-Table-Competition-Corrected) (eu + us) | Structure (cell adjacency) | 0.9522 | 0.9414 | 0.9468 |
| [SciTSR](https://github.com/Academic-Hammer/SciTSR) test (3,000 tables) | Structure (cell adjacency, macro) | 0.9254 | 0.8997 | 0.9124 |

## Installation

**Rust:**

```sh
cargo add pdfni
```

**CLI:**

```sh
cargo install pdfni
```

**Python (3.10+, no runtime dependencies):**

```sh
pip install pdfni
```

## Quick Start

### Rust

```rust
use pdfni::{ExtractOptions, extract_from_bytes};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read("input.pdf")?;
    let doc = extract_from_bytes(&bytes, None, &ExtractOptions::default())?;
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}
```

For reading-order blocks, use `extract_content_from_bytes` or `extract_document_from_bytes`. With the opt-in `markdown` feature enabled, `extract_markdown_from_bytes` is also available.

### CLI

```sh
# Extract tables only (Document JSON)
pdfni --output tables input.pdf output.json

# Extract ruled tables from selected pages to stdout
pdfni --output tables --mode ruled --pages 1,3-5 input.pdf

# Default: text blocks and tables in reading order (content JSON)
pdfni input.pdf output.json
```

Use `--output doc` for the full document model. See [docs/CLI.md](https://github.com/honononta-2/pdfni/blob/master/docs/CLI.md) for `--region`, `--detect-header-footer`, and encrypted PDFs.

### Python

```python
import pdfni

doc = pdfni.extract_tables_from_pdf("input.pdf")
for page in doc["pages"]:
    for table in page["tables"]:
        print([[cell["text"] for cell in row] for row in table["data"]])
```

For reading-order text blocks and tables together, use `extract_content_from_pdf` or `extract_document_from_pdf`. See [docs/PYTHON.md](https://github.com/honononta-2/pdfni/blob/master/docs/PYTHON.md) for keyword arguments (`pages`, `region`, `mode`, `password`), return shapes, and exceptions.

### WebAssembly (Browser & Node.js)

Build the package with `wasm-pack`:

```sh
wasm-pack build --release --target web
```

```js
import init, { extract_tables_from_pdf } from "./pkg/pdfni.js";

await init();
const bytes = new Uint8Array(await file.arrayBuffer());
const doc = JSON.parse(extract_tables_from_pdf(bytes));
```

See [docs/WASM.md](https://github.com/honononta-2/pdfni/blob/master/docs/WASM.md) for Node.js targets, feature flags, and options.

## Documentation

- [Building and feature flags](https://github.com/honononta-2/pdfni/blob/master/docs/BUILD.md)
- [Python](https://github.com/honononta-2/pdfni/blob/master/docs/PYTHON.md)
- [CLI usage](https://github.com/honononta-2/pdfni/blob/master/docs/CLI.md)
- [WebAssembly (Node.js and browser)](https://github.com/honononta-2/pdfni/blob/master/docs/WASM.md)
- [C ABI (native)](https://github.com/honononta-2/pdfni/blob/master/docs/FFI.md)
- [Architecture and internals](https://github.com/honononta-2/pdfni/blob/master/docs/ARCHITECTURE.md)
- [API reference on docs.rs](https://docs.rs/pdfni)

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](https://github.com/honononta-2/pdfni/blob/master/LICENSE).

## Third-Party Notices

The bundled PDF reader is adapted from [Mozilla pdf.js](https://github.com/mozilla/pdf.js), and the bundled binary CMap data is derived from [Adobe CMap resources](https://github.com/adobe-type-tools/cmap-resources). See [NOTICE](https://github.com/honononta-2/pdfni/blob/master/NOTICE) for attribution and license details.
