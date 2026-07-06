// pdf.js ルートの入口

import initWasm, { extract_tables } from '../pkg/reticella.js';
import { readParts } from './reader-pdfjs.mjs';

let ready;

// wasmUrl 省略時は pkg 同梱の .wasm を new URL で解決
export function init(wasmUrl) {
  ready = initWasm(wasmUrl ? { module_or_path: wasmUrl } : undefined);
  return ready;
}

// PDF バイト列から表を抽出する
// pdfjs: pdfjs-dist のモジュール、opts: { cMapUrl, standardFontDataUrl, options }
export async function extractTables(pdfjs, pdfBytes, opts = {}) {
  await (ready ?? init());
  const task = pdfjs.getDocument({
    data: pdfBytes,
    cMapUrl: opts.cMapUrl,
    cMapPacked: true,
    standardFontDataUrl: opts.standardFontDataUrl,
    isEvalSupported: false,
  });
  const doc = await task.promise;
  try {
    const pages = await readParts(doc, pdfjs.OPS);
    return JSON.parse(extract_tables(JSON.stringify({ pages, options: opts.options })));
  } finally {
    await task.destroy();
  }
}
