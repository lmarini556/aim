/* Deterministic 12x12 pixel-art agent sprite, seeded by session_id.
 * Each sprite mixes: body template, palette, eyes, mouth, accessory, accent stripe. */

const SPRITE_GRID = 12;

/* ---------- seeded PRNG ---------- */
function hashStr(s) {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619) >>> 0;
  }
  return h >>> 0;
}
function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const pick = (rng, arr) => arr[Math.floor(rng() * arr.length)];

/* ---------- palettes (body, shadow, accent, eye, accessory) ---------- */
const PALETTES = [
  { body: "#ff7a2a", shadow: "#b8430a", accent: "#ffd4a3", eye: "#1a0a00", acc: "#2a1a0a" },
  { body: "#7c5cff", shadow: "#4527a8", accent: "#d6c9ff", eye: "#0e0033", acc: "#1a0a4a" },
  { body: "#22c55e", shadow: "#15803d", accent: "#bbf7d0", eye: "#052e16", acc: "#0f3d20" },
  { body: "#06b6d4", shadow: "#0e7490", accent: "#a5f3fc", eye: "#042f3a", acc: "#0a3f4a" },
  { body: "#f472b6", shadow: "#9d174d", accent: "#fce7f3", eye: "#3a0a24", acc: "#5a1a3a" },
  { body: "#facc15", shadow: "#a16207", accent: "#fef9c3", eye: "#3a2a00", acc: "#5a3a0a" },
  { body: "#ef4444", shadow: "#991b1b", accent: "#fecaca", eye: "#3a0a0a", acc: "#5a1a1a" },
  { body: "#e2e8f0", shadow: "#64748b", accent: "#ffffff", eye: "#0f172a", acc: "#1e293b" },
  { body: "#a78bfa", shadow: "#5b21b6", accent: "#ede9fe", eye: "#1e0a4a", acc: "#2a1a5a" },
  { body: "#34d399", shadow: "#047857", accent: "#d1fae5", eye: "#022c22", acc: "#0a3a30" },
];

/* ---------- body templates (12x12, "." transparent, "B" body, "S" shadow) ---------- */
const BODIES = [
  // 0: blob
  [
    "............",
    "...BBBBBB...",
    "..BBBBBBBB..",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    "..BBBBBBBB..",
    "...SSSSSS...",
    "............",
    "............",
  ],
  // 1: tall capsule
  [
    "............",
    "...BBBBBB...",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BSSSSSSB..",
    "...SSSSSS...",
  ],
  // 2: square robot
  [
    "............",
    "............",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..BBBBBBBB..",
    "..SSSSSSSS..",
    "............",
  ],
  // 3: wide bean
  [
    "............",
    "............",
    "............",
    ".BBBBBBBBBB.",
    "BBBBBBBBBBBB",
    "BBBBBBBBBBBB",
    "BBBBBBBBBBBB",
    "BBBBBBBBBBBB",
    "BBBBBBBBBBBB",
    ".BBBBBBBBBB.",
    "..SSSSSSSS..",
    "............",
  ],
  // 4: ghost
  [
    "............",
    "...BBBBBB...",
    "..BBBBBBBB..",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    ".BBBBBBBBBB.",
    ".B.BB.BB.BB.",
    "...S..S..S..",
  ],
  // 5: diamond
  [
    "............",
    "............",
    ".....BB.....",
    "....BBBB....",
    "...BBBBBB...",
    "..BBBBBBBB..",
    ".BBBBBBBBBB.",
    "..BBBBBBBB..",
    "...BBBBBB...",
    "....SSSS....",
    ".....SS.....",
    "............",
  ],
];

/* ---------- helpers ---------- */
function cloneBody(b) {
  return b.map((row) => row.split(""));
}
function bodyCells(grid) {
  const cells = [];
  for (let y = 0; y < grid.length; y++) {
    for (let x = 0; x < grid[y].length; x++) {
      if (grid[y][x] === "B") cells.push([x, y]);
    }
  }
  return cells;
}
function bbox(cells) {
  let minX = 99, maxX = -1, minY = 99, maxY = -1;
  for (const [x, y] of cells) {
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y < minY) minY = y;
    if (y > maxY) maxY = y;
  }
  return { minX, maxX, minY, maxY };
}
function setIfBody(grid, x, y, ch) {
  if (y < 0 || y >= grid.length || x < 0 || x >= grid[0].length) return false;
  if (grid[y][x] === "B" || grid[y][x] === "S" || grid[y][x] === "A") {
    grid[y][x] = ch;
    return true;
  }
  return false;
}
function setAny(grid, x, y, ch) {
  if (y < 0 || y >= grid.length || x < 0 || x >= grid[0].length) return false;
  grid[y][x] = ch;
  return true;
}

/* ---------- features (E=eye, P=pupil, M=mouth, A=accent stripe, H=accessory) ---------- */
function drawEyes(grid, kind, box) {
  const eyeY = box.minY + Math.max(2, Math.floor((box.maxY - box.minY) * 0.35));
  const cx = (box.minX + box.maxX) / 2;
  const lx = Math.floor(cx - 2);
  const rx = Math.floor(cx + 2);
  if (kind === 0) {
    // single dots
    setIfBody(grid, lx, eyeY, "E");
    setIfBody(grid, rx, eyeY, "E");
  } else if (kind === 1) {
    // big eyes with pupil
    setIfBody(grid, lx - 1, eyeY, "E");
    setIfBody(grid, lx, eyeY, "E");
    setIfBody(grid, rx, eyeY, "E");
    setIfBody(grid, rx + 1, eyeY, "E");
    setIfBody(grid, lx, eyeY, "P");
    setIfBody(grid, rx, eyeY, "P");
  } else if (kind === 2) {
    // visor — full horizontal stripe of eye color
    for (let x = box.minX + 1; x <= box.maxX - 1; x++) setIfBody(grid, x, eyeY, "E");
  } else if (kind === 3) {
    // angry slits
    setIfBody(grid, lx - 1, eyeY, "E");
    setIfBody(grid, lx, eyeY, "E");
    setIfBody(grid, rx, eyeY, "E");
    setIfBody(grid, rx + 1, eyeY, "E");
  } else if (kind === 4) {
    // tall pupils
    setIfBody(grid, lx, eyeY, "E");
    setIfBody(grid, lx, eyeY + 1, "E");
    setIfBody(grid, rx, eyeY, "E");
    setIfBody(grid, rx, eyeY + 1, "E");
  }
}

function drawMouth(grid, kind, box) {
  const my = box.minY + Math.max(4, Math.floor((box.maxY - box.minY) * 0.65));
  const cx = Math.floor((box.minX + box.maxX) / 2);
  if (kind === 0) {
    // smile
    setIfBody(grid, cx - 1, my, "M");
    setIfBody(grid, cx, my, "M");
    setIfBody(grid, cx + 1, my, "M");
    setIfBody(grid, cx - 2, my - 1, "M");
    setIfBody(grid, cx + 2, my - 1, "M");
  } else if (kind === 1) {
    // flat line
    setIfBody(grid, cx - 1, my, "M");
    setIfBody(grid, cx, my, "M");
    setIfBody(grid, cx + 1, my, "M");
  } else if (kind === 2) {
    // open square mouth
    setIfBody(grid, cx - 1, my, "M");
    setIfBody(grid, cx, my, "M");
    setIfBody(grid, cx + 1, my, "M");
    setIfBody(grid, cx - 1, my + 1, "M");
    setIfBody(grid, cx + 1, my + 1, "M");
    setIfBody(grid, cx - 1, my + 2, "M");
    setIfBody(grid, cx, my + 2, "M");
    setIfBody(grid, cx + 1, my + 2, "M");
  } else if (kind === 3) {
    // tongue out
    setIfBody(grid, cx, my, "M");
    setIfBody(grid, cx, my + 1, "M");
  }
}

function drawAccent(grid, kind, box) {
  if (kind === 0) return;
  if (kind === 1) {
    // belt
    const y = box.maxY - 1;
    for (let x = box.minX; x <= box.maxX; x++) {
      if (grid[y] && grid[y][x] === "B") grid[y][x] = "A";
    }
  } else if (kind === 2) {
    // back stripe (vertical center)
    const cx = Math.floor((box.minX + box.maxX) / 2);
    for (let y = box.minY; y <= box.maxY; y++) {
      if (grid[y][cx] === "B") grid[y][cx] = "A";
    }
  } else if (kind === 3) {
    // cheek dots (random-ish)
    const cy = box.minY + Math.floor((box.maxY - box.minY) * 0.55);
    setIfBody(grid, box.minX + 1, cy, "A");
    setIfBody(grid, box.maxX - 1, cy, "A");
  }
}

function drawAccessory(grid, kind, box) {
  const cx = Math.floor((box.minX + box.maxX) / 2);
  const top = box.minY;
  if (kind === 0) return;
  if (kind === 1) {
    // single antenna
    setAny(grid, cx, top - 1, "H");
    setAny(grid, cx, top - 2, "H");
    setAny(grid, cx, top - 3, "E");
  } else if (kind === 2) {
    // double antenna
    setAny(grid, cx - 2, top - 1, "H");
    setAny(grid, cx - 2, top - 2, "H");
    setAny(grid, cx + 2, top - 1, "H");
    setAny(grid, cx + 2, top - 2, "H");
  } else if (kind === 3) {
    // top hat
    for (let x = cx - 2; x <= cx + 2; x++) setAny(grid, x, top - 1, "H");
    for (let x = cx - 1; x <= cx + 1; x++) setAny(grid, x, top - 2, "H");
    setAny(grid, cx - 1, top - 3, "H");
    setAny(grid, cx, top - 3, "H");
    setAny(grid, cx + 1, top - 3, "H");
  } else if (kind === 4) {
    // horns
    setAny(grid, cx - 3, top, "H");
    setAny(grid, cx - 3, top - 1, "H");
    setAny(grid, cx + 3, top, "H");
    setAny(grid, cx + 3, top - 1, "H");
  } else if (kind === 5) {
    // crown
    setAny(grid, cx - 2, top - 1, "H");
    setAny(grid, cx, top - 1, "H");
    setAny(grid, cx + 2, top - 1, "H");
    for (let x = cx - 2; x <= cx + 2; x++) setAny(grid, x, top, "H");
  }
}

/* ---------- render to SVG ---------- */
function renderSvg(grid, palette, bg) {
  const h = grid.length;
  const w = grid[0].length;
  const colors = {
    B: palette.body,
    S: palette.shadow,
    A: palette.accent,
    E: palette.eye,
    P: palette.shadow,
    M: palette.eye,
    H: palette.acc,
  };
  const rects = [];
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const c = grid[y][x];
      const fill = colors[c];
      if (!fill) continue;
      rects.push(`<rect x="${x}" y="${y}" width="1" height="1" fill="${fill}"/>`);
    }
  }
  const bgRect = bg ? `<rect width="${w}" height="${h}" fill="${bg}"/>` : "";
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${w} ${h}" shape-rendering="crispEdges" style="image-rendering:pixelated">${bgRect}${rects.join("")}</svg>`;
}

/* ---------- public api ---------- */
function agentSprite(seed) {
  const rng = mulberry32(hashStr(seed || "x"));
  const palette = pick(rng, PALETTES);
  const body = pick(rng, BODIES);
  const eyeKind = Math.floor(rng() * 5);
  const mouthKind = Math.floor(rng() * 4);
  const accentKind = Math.floor(rng() * 4);
  const accessoryKind = Math.floor(rng() * 6);

  const grid = cloneBody(body);
  const box = bbox(bodyCells(grid));
  drawAccent(grid, accentKind, box);
  drawEyes(grid, eyeKind, box);
  drawMouth(grid, mouthKind, box);
  drawAccessory(grid, accessoryKind, box);
  return renderSvg(grid, palette);
}

window.agentSprite = agentSprite;
