# Building and feature flags

## Requirements

- Rust ≥ 1.85 (edition 2024)
- For wasm builds: `rustup target add wasm32-unknown-unknown`
- For browser packaging: [`wasm-pack`](https://rustwasm.github.io/wasm-pack/)

## Default build

```sh
cargo build --release
cargo test --features markdown
```

The default features enable the document stack (`document`), including the CLI,
the document pipeline, and all bundled CJK CMap groups. Markdown output is
opt-in (`markdown`); tests that check the document model through its Markdown
output only run with that feature.

## Feature flags

pdfni is organized into layered features. Higher layers pull in the layers
below them, so you only need to enable the top layer you want.

| Feature | Enables | Depends on |
| --- | --- | --- |
| `markdown` | Markdown output from the document model (`extract_markdown_from_bytes`) | `document` |
| `document` (default) | Document blocks + content view (`extract_document_from_bytes`, `extract_content_from_bytes`) | `reader` |
| `reader` | PDF byte input (`extract_from_bytes`, `extract_text_from_bytes`, `read_pages`, CLI) | `text` |
| `ffi` | Native C ABI entry points (`pdfni_extract_tables_json`, ...). Not available on wasm — see [FFI.md](FFI.md) | `reader` |
| `python` | Python extension module entry points. Not available on wasm — see [PYTHON.md](PYTHON.md) | `document` |
| `text` | Coordinate-tagged text (`TextDoc`, items, lines) | — |
| _(none)_ | Table detection from parts JSON (`extract_from_parts`) — always available | — |
| `cmap-japan1` (default) | Predefined CJK CMap group Adobe-Japan1 | — |
| `cmap-gb1` (default) | Predefined CJK CMap group Adobe-GB1 | — |
| `cmap-cns1` (default) | Predefined CJK CMap group Adobe-CNS1 | — |
| `cmap-korea1` (default) | Predefined CJK CMap group Adobe-Korea1 | — |

The `cmap-*` features are independent of `reader` so you can pick only the
scripts you need. `Identity-H` / `Identity-V` (used by most modern CJK PDFs
that carry ToUnicode) work without any `cmap-*` feature.

**CMap trimming warning.** If you disable all `cmap-*` features, PDFs that use
predefined CJK CMap names (`90ms-RKSJ-H`, `UniJIS-UTF16-H`, `GB-EUC-H`,
`UniCNS-UTF16-H`, `KSCms-UHC-H`, ...) will return `Error::Reader("Unknown CMap
name: ...")`. Latin PDFs and PDFs using `Identity-H` continue to work.

## Common build shapes

```sh
# Document stack (same as default)
cargo build --release

# Document stack plus Markdown output
cargo build --release --features markdown

# Document model, only Japan1
cargo build --release --no-default-features --features "document cmap-japan1"

# PDF byte input, coordinate text and tables, no document model
cargo build --release --no-default-features --features reader

# Coordinate text only (no PDF reader, no document model) — parts JSON only
cargo build --release --no-default-features --features text

# Table detection only (parts JSON in, tables out) — smallest build
cargo build --release --no-default-features

# Native shared library with the C ABI entry points
cargo build --release --features ffi
```

## wasm builds

```sh
# wasm with PDF byte input
cargo build --lib --target wasm32-unknown-unknown \
  --no-default-features --features reader

# wasm with the document model
cargo build --lib --target wasm32-unknown-unknown \
  --no-default-features --features document

# wasm with the document model and Markdown output
cargo build --lib --target wasm32-unknown-unknown \
  --no-default-features --features markdown

# wasm, detection only (parts JSON in, tables out — smallest wasm)
cargo build --lib --target wasm32-unknown-unknown --no-default-features
```

## Browser package (wasm-pack)

The browser smoke page lives under `demo/`. Build the package with the
default features plus `markdown` (the page imports document-level APIs and
the Markdown export) and serve it:

```sh
wasm-pack build --release --target web --out-dir demo/pkg --features markdown
node demo/server.mjs   # http://localhost:8080
```

## Python package (maturin)

The Python extension module is built with [maturin](https://www.maturin.rs/),
which enables the `python` feature through `pyproject.toml`:

```sh
maturin develop --release   # build and install into the active virtualenv
pytest pytests
```

See [PYTHON.md](PYTHON.md) for the Python API.
