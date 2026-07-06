// pdf.js のページから文字と罫線を取り出し wasm 入口用の parts に整える
// 座標は top-down、bbox は left/right/top/bottom

const DRAW_MOVE = 0, DRAW_LINE = 1, DRAW_CURVE = 2, DRAW_QUAD = 3, DRAW_CLOSE = 4;
const EPS = 0.8; // 軸平行判定の許容差

function mulMatrix(m, n) {
  return [
    m[0] * n[0] + m[2] * n[1], m[1] * n[0] + m[3] * n[1],
    m[0] * n[2] + m[2] * n[3], m[1] * n[2] + m[3] * n[3],
    m[0] * n[4] + m[2] * n[5] + m[4], m[1] * n[4] + m[3] * n[5] + m[5],
  ];
}
function applyMatrix(m, x, y) {
  return [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]];
}
function isSpace(c) {
  return c === ' ' || c === ' ' || c === '　';
}

// 1ページ分の文字を按分で1文字ずつの bbox にする
function readGlyphs(textContent, pageH) {
  const styles = textContent.styles || {};
  const glyphs = [];
  for (const item of textContent.items) {
    if (item.type) continue; // marked content は除く
    const str = item.str;
    if (!str) continue;
    const t = item.transform;
    const fontSize = Math.hypot(t[2], t[3]);
    const st = styles[item.fontName] || {};
    let asc = typeof st.ascent === 'number' ? st.ascent : 0.88;
    let desc = typeof st.descent === 'number' ? st.descent : -0.12;
    if (asc <= 0) asc = 0.88;
    if (desc >= 0) desc = -0.12;

    const chars = Array.from(str);
    const n = chars.length;
    const perW = item.width / n;
    const dlen = Math.hypot(t[0], t[1]) || 1;
    const ux = t[0] / dlen;
    const topY = t[5] + asc * fontSize;
    const botY = t[5] + desc * fontSize;
    for (let k = 0; k < n; k++) {
      const ch = chars[k];
      if (ch === '\n' || ch === '\r' || ch === '\t') continue;
      // pdf.js が列間の空隙を埋める幅広の合成スペースは列右端を次列へ膨らませるため除く
      if (isSpace(ch) && perW > fontSize) continue;
      const ox = t[4] + ux * perW * k;
      const ex = t[4] + ux * perW * (k + 1);
      glyphs.push({
        ch,
        left: Math.min(ox, ex),
        right: Math.max(ox, ex),
        top: pageH - topY,
        bottom: pageH - botY,
      });
    }
  }
  return glyphs;
}

// constructPath から軸平行な線分を取り出す
function readEdges(opList, OPS, pageH) {
  const edges = [];
  let ctm = [1, 0, 0, 1, 0, 0];
  const stack = [];
  const pushAxisEdge = (ax, ay, bx, by) => {
    const [x0u, y0u] = applyMatrix(ctm, ax, ay);
    const [x1u, y1u] = applyMatrix(ctm, bx, by);
    const dx = x1u - x0u, dy = y1u - y0u;
    if (Math.abs(dx) <= EPS && Math.abs(dy) > EPS) {
      edges.push({ orientation: 'vertical', left: x1u, right: x1u, top: pageH - Math.max(y0u, y1u), bottom: pageH - Math.min(y0u, y1u) });
    } else if (Math.abs(dy) <= EPS && Math.abs(dx) > EPS) {
      const yy = pageH - y1u;
      edges.push({ orientation: 'horizontal', left: Math.min(x0u, x1u), right: Math.max(x0u, x1u), top: yy, bottom: yy });
    }
  };

  for (let i = 0; i < opList.fnArray.length; i++) {
    const fn = opList.fnArray[i];
    const args = opList.argsArray[i];
    if (fn === OPS.save) {
      stack.push(ctm.slice());
    } else if (fn === OPS.restore) {
      ctm = stack.pop() || [1, 0, 0, 1, 0, 0];
    } else if (fn === OPS.transform) {
      ctm = mulMatrix(ctm, args);
    } else if (fn === OPS.paintFormXObjectBegin) {
      // Form XObject の行列の適用
      stack.push(ctm.slice());
      if (args && args[0]) ctm = mulMatrix(ctm, args[0]);
    } else if (fn === OPS.paintFormXObjectEnd) {
      ctm = stack.pop() || [1, 0, 0, 1, 0, 0];
    } else if (fn === OPS.constructPath) {
      const buf = args[1] && args[1][0];
      if (!buf) continue;
      let j = 0, sx = 0, sy = 0, px = 0, py = 0;
      while (j < buf.length) {
        const op = buf[j++];
        if (op === DRAW_MOVE) {
          sx = px = buf[j++]; sy = py = buf[j++];
        } else if (op === DRAW_LINE) {
          const x = buf[j++], y = buf[j++];
          pushAxisEdge(px, py, x, y);
          px = x; py = y;
        } else if (op === DRAW_CURVE) {
          j += 6; px = buf[j - 2]; py = buf[j - 1];
        } else if (op === DRAW_QUAD) {
          j += 4; px = buf[j - 2]; py = buf[j - 1];
        } else if (op === DRAW_CLOSE) {
          pushAxisEdge(px, py, sx, sy);
          px = sx; py = sy;
        } else {
          break;
        }
      }
    }
  }
  return edges;
}

// pdf.js の1ページを parts に変換する
export async function readPage(page, OPS) {
  const view = page.view;
  const pageH = view[3] - view[1];
  const pageW = view[2] - view[0];
  const textContent = await page.getTextContent({ disableNormalization: true, includeMarkedContent: false });
  const opList = await page.getOperatorList();
  return {
    width: pageW,
    height: pageH,
    glyphs: readGlyphs(textContent, pageH),
    edges: readEdges(opList, OPS, pageH),
  };
}

// pdf.js の全ページを wasm 入口用の parts 配列に変換する
export async function readParts(pdfDocument, OPS) {
  const pages = [];
  for (let p = 1; p <= pdfDocument.numPages; p++) {
    const page = await pdfDocument.getPage(p);
    pages.push(await readPage(page, OPS));
  }
  return pages;
}
