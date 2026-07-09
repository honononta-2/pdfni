// pdfium-wasm ルートの入口
// 別途 Emscripten ビルドの pdfium.wasm とグルーを読み込み initialize_pdfium_render で束ねる
// 詳細は pdfium-render の examples/index.html を参照

import initWasm, * as core from '../pkg-pdfium/reticella.js';

// 初回 init の完了を共有する Promise
let initializing;
let ready = false;

// wasm コアと pdfium モジュールを束ねて初期化する
// pdfiumModule: pdfium.wasm のグルーが公開する Emscripten Module オブジェクト
// wasmUrl: pkg-pdfium/reticella_bg.wasm の場所、省略時は同梱を new URL で解決
// ローカルモジュールはオブジェクトである必要がある
export function init(opts = {}) {
  initializing ??= doInit(opts).then(
    () => {
      ready = true;
    },
    (e) => {
      initializing = undefined;
      throw e;
    },
  );
  return initializing;
}

async function doInit({ pdfiumModule, wasmUrl, debug = false }) {
  const local = await initWasm(wasmUrl ? { module_or_path: wasmUrl } : undefined);
  if (!core.initialize_pdfium_render(pdfiumModule, local, debug)) {
    throw new Error('initialize_pdfium_render に失敗しました');
  }
}

// PDF バイト列から表を抽出する
export function extractTables(pdfBytes, options, password) {
  if (!ready) throw new Error('先に init() を呼んでください');
  const json = core.extract_tables_from_pdf(
    pdfBytes,
    options ? JSON.stringify(options) : undefined,
    password ?? undefined,
  );
  return JSON.parse(json);
}
