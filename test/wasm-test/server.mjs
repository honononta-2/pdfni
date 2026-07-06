// Static server for this directory. Serves .wasm with the correct MIME type.
import { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = fileURLToPath(new URL('.', import.meta.url));
const PORT = process.env.PORT ? Number(process.env.PORT) : 8080;

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
  '.json': 'application/json; charset=utf-8',
  '.bcmap': 'application/octet-stream',
  '.pdf': 'application/pdf',
  '.pfb': 'application/octet-stream',
  '.ttf': 'font/ttf',
};

const server = createServer(async (req, res) => {
  try {
    const urlPath = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    let rel = normalize(urlPath).replace(/^(\.\.[/\\])+/, '');
    if (rel === '/' || rel === '\\') rel = '/index.html';
    const file = join(ROOT, rel);
    if (!file.startsWith(ROOT)) { res.writeHead(403).end('forbidden'); return; }
    const st = await stat(file).catch(() => null);
    if (!st || !st.isFile()) { res.writeHead(404).end('not found'); return; }
    const body = await readFile(file);
    res.writeHead(200, { 'content-type': MIME[extname(file).toLowerCase()] || 'application/octet-stream' });
    res.end(body);
  } catch (e) {
    res.writeHead(500).end(String(e));
  }
});

server.listen(PORT, () => {
  console.log(`serving http://localhost:${PORT}/ (Ctrl+C to stop)`);
});
