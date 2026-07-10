# reticella (JS/wasm)

テキスト埋め込みPDFから表を抽出しJSONで返す。読み取りも検出も純Rustリーダー（reader）の wasm ビルドで行い、pdf.js や pdfium などの外部PDFライブラリは不要。

## ビルド

```
wasm-pack build --release --target web --out-dir pkg --no-default-features --features reader
```

## 使い方

```js
import init, { extract_tables_from_pdf } from './pkg/reticella.js';

await init(); // 既定で pkg/reticella_bg.wasm を解決する
const json = extract_tables_from_pdf(pdfBytes, undefined, undefined); // 第2引数: オプションJSON、第3引数: パスワード（省略可）
const doc = JSON.parse(json);
```

外部リーダーで parts（文字・線分）を用意して検出コアだけ使う場合は `extract_tables(inputJson)` を使う（parts の契約は `specs/01-parts-contract.md`）。

## 配信

wasm は生ファイルのまま配置し、サーバの `Content-Encoding: br`（または gzip）で圧縮配信する。base64 埋め込みは避ける（サイズ増・ストリーミング不可）。
