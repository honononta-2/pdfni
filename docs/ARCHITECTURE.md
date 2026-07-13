# Architecture

## Pipeline

```
PDF bytes
  → pure-Rust reader (glyphs, edges, graphics; optional text chars)
  → table detect: lattice → hybrid → text (by DetectMode)
  → Document JSON

  ↘ TextDoc → items / lines
  ↘ DocDoc (blocks + tables) → Markdown
```

The document entry point (`extract_document_from_bytes`) builds the parts for
table detection and the coordinate-tagged text in a single content-stream pass
per page, so the underlying reader is not run twice.

## Coordinates

Coordinates are top-down, in PDF points (1/72 inch). The origin is the top-left
of the page's view rectangle — the intersection of `CropBox` and `MediaBox`,
falling back to `MediaBox` when the intersection is empty or invalid — after
the page `/Rotate` is applied. Pages that carry a single dominant rotation
(90 / 180 / 270 degrees) are normalized inside the reader; the detected table
bounding boxes are returned in the original page coordinate frame.

## Table detectors

Three detectors run in sequence under `DetectMode::Auto`. Regions already
covered by a confirmed table are removed before the next detector runs, so
they do not overlap.

| Detector | What it looks at | Best for |
| --- | --- | --- |
| `lattice` | Axis-aligned ruled edges | Tables with visible ruling |
| `hybrid` | Ruled edges + text alignment | Partially ruled tables |
| `text` | Text alignment only | Borderless tables |

`DetectMode::Ruled` runs `lattice` only. `DetectMode::Borderless` runs `text`
only.

## Reader lineage

The PDF reader under `src/reader/` is ported in part from
[Mozilla pdf.js](https://github.com/mozilla/pdf.js) and substantially modified
for this library. Bundled encodings, glyph list, standard font metrics, and
binary CMaps under `src/reader/data/` are generated from **pdf.js 6.0.227**.
CMap binaries are derived from
[Adobe CMap resources](https://github.com/adobe-type-tools/cmap-resources)
(via pdf.js `external/bcmaps`). License details are in the `NOTICE` file at
the crate root.

## Encrypted PDFs

reticella implements the PDF Standard security handler for revisions R2 – R6
with the `/V` values 1 / 2 / 4 / 5. This covers RC4 and AES-128 / AES-256.
The optional `password` argument on the byte-input entry points is passed to
the handler; an empty or missing password is tried first.

Errors:

- Wrong or missing password → `Error::BadPassword`
- Non-standard security handler → `Error::Reader("unknown encryption method: ...")`

## Parallelism

On native, pages are processed in parallel (see `src/par.rs`). On wasm, pages
are processed sequentially. The output order and content match across both
targets — the wasm build is a strict subset of the native behavior.

## Directory layout

```
src/
  reader/          pure-Rust PDF reader (+ data tables from pdf.js / Adobe CMaps)
  extract.rs       parts → table detection orchestration
  detect_*.rs      lattice / hybrid / text detectors
  text.rs          TextDoc, items, lines
  document.rs      DocDoc, headings/lists, Markdown
  columns.rs       multi-column layout for the document model
  wasm.rs          JS exports
  bin/main.rs      CLI (tables only)
examples/          reader_probe, text_probe, ...
test/wasm-test/    browser smoke page
data/              source .bcmap files used when regenerating bundled data
```
