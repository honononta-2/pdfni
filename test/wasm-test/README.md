# wasm-test

Browser test page for both wasm routes: the pdf.js route and the pdfium-wasm route.
Open a PDF in the page and see the detected tables.

Generated artifacts and third-party files are not committed; produce them with the steps below.

## 1. Build the wasm bindings

From the crate root (`reticella/`):

```sh
# pd.js core (detection only), for the pdf.js route
wasm-pack build --target web --out-dir test/wasm-test/pkg --no-default-features

# pdfium route, no-modules build (matches the pdfium-render example wiring)
wasm-pack build --target no-modules --out-dir test/wasm-test/pkg-pdfium-nomod --no-default-features --features pdfium
```

## 2. pdf.js assets (for the pdf.js route)

Copy from an installed `pdfjs-dist` (6.x) into `test/wasm-test/vendor/`:

- `build/pdf.mjs`        -> `vendor/pdfjs/pdf.mjs`
- `build/pdf.worker.mjs` -> `vendor/pdfjs/pdf.worker.mjs`
- `cmaps/`               -> `vendor/cmaps/`
- `standard_fonts/`      -> `vendor/standard_fonts/`

## 3. pdfium engine (for the pdfium-wasm route)

Download a pdfium-render-compatible Emscripten build from
paulocoutinhox/pdfium-lib releases (tested with release `7902`) and place:

- `release/node/pdfium.js`   -> `vendor/pdfium/pdfium.js`
- `release/node/pdfium.wasm` -> `vendor/pdfium/pdfium.wasm`

The glue must expose the global factory `PDFiumModule()`.

## 4. Run

```sh
node test/wasm-test/server.mjs      # http://localhost:8080 (PORT env to change)
```

Open http://localhost:8080/, choose a PDF, and click Extract in either section.

## Notes

- The static server sets `application/wasm` for `.wasm`, needed for streaming compilation.
- pdfium's local module passed to `initialize_pdfium_render` must be an object (the resolved
  init output), not the `wasm_bindgen` function, which fails the `is_object()` check.
