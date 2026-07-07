# reticella (JS/wasm)

テキスト埋め込みPDFから表を抽出しJSONで返す。読み取り方式で2つのルートがあり、主従はない。

- `reticella/pdfjs` — pdf.js で読み取り、検出は wasm コア。ブラウザ・Node.js どちらでも動く。コア約341KB（Brotli約95KB）。pdf.js の座標近似があるため罫線なし表で挙動差が出うる
- `reticella/pdfium` — pdfium を wasm で動かし読み取りも検出も行う。読み取り実装は pdfium ルートの Rust 実装（`src/reader.rs`）と共通。別途 pdfium.wasm（Emscripten ビルド）が要る

## pdf.js ルート

pdf.js は peer 依存（`pdfjs-dist` 6系）。日本語PDFのため cMaps の指定が必須。

```js
import * as pdfjs from 'pdfjs-dist';
import { extractTables } from 'reticella/pdfjs';

pdfjs.GlobalWorkerOptions.workerSrc = /* pdf.worker.min.mjs の URL */;

const result = await extractTables(pdfjs, pdfBytes, {
  cMapUrl: '/cmaps/',              // pdfjs-dist/cmaps を配置
  standardFontDataUrl: '/standard_fonts/',
});
```

wasm は既定で `pkg/reticella_bg.wasm` を `new URL(..., import.meta.url)` で解決する。場所を変える場合は `init(wasmUrl)` を先に呼ぶ。

## pdfium ルート

pdfium-render の wasm 連携を使う。別途 Emscripten ビルドの pdfium.wasm とそのグルーを読み込み、その Module オブジェクトを渡す。手順は pdfium-render の `examples/index.html` に準じる。

```js
import { init, extractTables } from 'reticella/pdfium';

// pdfium.wasm のグルーが公開する Emscripten Module を取得しておく
await init({ pdfiumModule });
const result = await extractTables(pdfBytes);
```

## 配信

wasm は生ファイルのまま配置し、サーバの `Content-Encoding: br`（または gzip）で圧縮配信する。base64 埋め込みは避ける（サイズ増・ストリーミング不可）。
