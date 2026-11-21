# Python

pdfni is available as a Python package built on [PyO3](https://pyo3.rs/).
It needs Python 3.10 or newer and has no runtime dependencies.

```sh
pip install pdfni
```

```python
import pdfni

content = pdfni.extract_content_from_pdf("input.pdf", pages="1-3")
for page in content["pages"]:
    for block in page["blocks"]:
        if block["type"] == "table":
            print([[cell["text"] for cell in row] for row in block["data"]])
        else:
            print(block["kind"], block["text"])
```

## Functions

| Function | Returns |
| --- | --- |
| `extract_content_from_pdf(source, ...)` | `DocContent` — text blocks and tables in reading order, no per-glyph data |
| `extract_document_from_pdf(source, ...)` | `DocDoc` — blocks and tables with fonts and per-glyph data |
| `extract_tables_from_pdf(source, ...)` | table `Document` — the same shape as `extract_from_bytes` |
| `extract_text_from_pdf(source, ...)` | `TextDoc` — coordinate-tagged characters |
| `extract_text_items_from_pdf(source, ...)` | pdf.js-like items, one list per page |
| `extract_text_lines_from_pdf(source, ...)` | line / word view, one list per page |
| `extract_tables(parts)` | table `Document` from parts (glyphs and edges read by another PDF reader) |

Markdown output is not part of the Python package.

## Arguments

```python
def extract_content_from_pdf(
    source,
    *,
    password=None,
    pages=None,
    region=None,
    mode=None,
    detect_header_footer=None,
    options=None,
): ...
```

Everything after `source` is keyword-only.

| Argument | Type | Meaning |
| --- | --- | --- |
| `source` | `bytes`, `bytearray`, `memoryview`, `str`, `os.PathLike` | PDF bytes, or a file path when given a `str` / `os.PathLike` |
| `password` | `str` | password for encrypted PDFs |
| `pages` | `str` | 1-based page selection (`"1,3-5,7"` style) |
| `region` | `tuple[float, float, float, float]` | `(left, top, right, bottom)` in top-down display coordinates |
| `mode` | `"auto"`, `"ruled"`, `"borderless"` | table detectors to run |
| `detect_header_footer` | `bool` | mark repeated page headers and footers with a `role` |
| `options` | `dict` | any other setting, described below |

The text functions (`extract_text_from_pdf`, `extract_text_items_from_pdf`,
`extract_text_lines_from_pdf`) do not run table detection, so they take only
`source`, `password`, `pages`, `region` and `options`.

`extract_tables` takes a single `dict` with a `pages` list of parts and an
optional `options` dict, the same shape as the parts JSON accepted by the
wasm `extract_tables` export.

## `options`

`options` accepts the same keys as the `options_json` of the
[wasm](WASM.md) and [C ABI](FFI.md) entry points: the fields of
`ExtractOptions` (`mode`, `detect_lists`, `bidi`, `detect_header_footer`, the
`reader` section, ...) plus `pages` and `region`.

```python
pdfni.extract_content_from_pdf(
    "input.pdf",
    options={"detect_lists": False, "reader": {"max_decoded_bytes": 64 * 1024 * 1024}},
)
```

When a keyword argument and `options` set the same key, the keyword argument
wins. In `options`, `region` is a dict with `left` and `top` plus either
`right` / `bottom` or `width` / `height`.

## Return values

Results are plain `dict` / `list` objects with the same keys as the JSON
outputs of the CLI and the wasm exports. Fixed-length numeric arrays
(`transform` and `advance` on characters and items) are returned as `tuple`.

Non-fatal notices, such as a `region` given in both forms, are reported in
the result's `warnings` list. The key is absent when there are none.

## Exceptions

| Exception | Raised when |
| --- | --- |
| `pdfni.PdfniError` | the PDF cannot be read. Base class of the three below |
| `pdfni.BadPasswordError` | a password is required or incorrect |
| `pdfni.SecurityRestrictedError` | security restrictions prevent loading |
| `pdfni.InvalidFormatError` | the data is not in a supported PDF format |
| `ValueError` | `pages`, `region`, `mode` or `options` has an invalid value |
| `TypeError` | an argument has the wrong type |
| `OSError` | the file at `source` cannot be read (`FileNotFoundError` when missing) |

```python
try:
    content = pdfni.extract_content_from_pdf(path)
except pdfni.BadPasswordError:
    content = pdfni.extract_content_from_pdf(path, password=ask_password())
```

## Threads

Extraction releases the GIL, so other Python threads keep running while a
PDF is processed. Pages are processed in parallel inside the extension
module.

## Type hints

The package ships a stub file (`py.typed` is included), so editors and type
checkers see the signatures above. Result dictionaries are typed as
`dict[str, Any]`.

## Building from source

Building needs a Rust toolchain (≥ 1.85) and
[maturin](https://www.maturin.rs/).

```sh
python -m venv .venv && source .venv/bin/activate
pip install maturin pytest
maturin develop --release
pytest pytests
```

See [BUILD.md](BUILD.md) for the feature layering.
