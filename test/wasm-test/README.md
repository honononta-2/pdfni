# wasm-test

Browser test page for the reader wasm route (pure Rust reader + detection).
Open a PDF in the page and see the detected tables.

Generated artifacts are not committed; produce them with the steps below.

## 1. Build the wasm bindings

From the crate root (`pdfni/`):

```sh
wasm-pack build --release --target web --out-dir test/wasm-test/pkg --no-default-features --features reader
```

## 2. Run

```sh
node test/wasm-test/server.mjs      # http://localhost:8080 (PORT env to change)
```

Open http://localhost:8080/, choose a PDF, and click Extract.

## Notes

- The static server sets `application/wasm` for `.wasm`, needed for streaming compilation.
