// pdfium-wasm ルートの入口
// 別途 Emscripten ビルドの pdfium.wasm とグルーを読み込み initialize_pdfium_render で束ねる
// 詳細は pdfium-render の examples/index.html を参照

import initWasm, * as core from '../pkg-pdfium/reticella.js';

let ready;

// wasm コアと pdfium モジュールを束ねて初期化する
// pdfiumModule: pdfium.wasm のグルーが公開する Emscripten Module オブジェクト
// wasmUrl: pkg-pdfium/reticella_bg.wasm の場所、省略時は同梱を new URL で解決
// ローカルモジュールはオブジェクトである必要がある
export async function init({ pdfiumModule, wasmUrl, debug = false } = {}) {
  const local = await initWasm(wasmUrl ? { module_or_path: wasmUrl } : undefined);
  if (!core.initialize_pdfium_render(pdfiumModule, local, debug)) {
    throw new Error('initialize_pdfium_render に失敗しました');
  }
  ready = true;
}

// PDF バイト列から表を抽出する
export function extractTables(pdfBytes, options) {
  if (!ready) throw new Error('先に init() を呼んでください');
  const json = core.extract_tables_from_pdf(pdfBytes, options ? JSON.stringify(options) : undefined);
  return JSON.parse(json);
}
