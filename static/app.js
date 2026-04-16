// Visible build stamp — lets us tell at a glance (console + bottom-right
// chip) whether a refresh actually pulled the latest code. If you don't
// see "CIU-BUILD-44" in the console AND a small orange "44" chip in the
// bottom-right corner, the browser is serving cached JS.
const CIU_BUILD = "CIU-BUILD-67";
console.log(`[ciu] ${CIU_BUILD} loaded at`, new Date().toISOString());
(() => {
  const stamp = document.createElement("div");
  stamp.id = "ciuBuildStamp";
  stamp.textContent = CIU_BUILD.replace("CIU-BUILD-", "v");
  stamp.style.cssText = [
    "position: fixed",
    "bottom: 4px",
    "right: 4px",
    "z-index: 2147483647",
    "background: #ff6b1a",
    "color: #000",
    "font: 700 10px ui-monospace, SFMono-Regular, Menlo, monospace",
    "padding: 2px 6px",
    "border-radius: 3px",
    "pointer-events: none",
    "opacity: 0.75",
  ].join(";");
  (document.body || document.documentElement).appendChild(stamp);
})();

const state = {
  filter: "all",
  groupFilter: null,
  query: "",
  showEnded: false,
  instances: [],
  selectedSid: null,
  transcriptData: null,
  renderedSid: null,
  lastListSig: "",
  collapsedGroups: new Set(),
  pendingAcks: {},
};

const FRESH_WINDOW_SECONDS = 300;


function acknowledge(sid, hookTs) {
  if (!sid || !hookTs) return;
  const prev = state.pendingAcks[sid] || 0;
  if (hookTs <= prev) return;
  state.pendingAcks[sid] = hookTs;
  api("POST", `/api/instances/${sid}/ack`, { timestamp: hookTs }).catch(() => {});
}

function freshIntensity(inst) {
  if (!inst.alive || inst.status !== "idle") return 0;
  if (inst.last_event !== "Stop") return 0;
  const ts = inst.hook_timestamp;
  if (!ts) return 0;
  const serverAck = inst.ack_timestamp || 0;
  const localAck = state.pendingAcks[inst.session_id] || 0;
  const ack = Math.max(serverAck, localAck);
  if (ts <= ack) return 0;
  const age = Date.now() / 1000 - ts;
  if (age < 0) return 1;
  if (age >= FRESH_WINDOW_SECONDS) return 0;
  return 1 - age / FRESH_WINDOW_SECONDS;
}

const $ = (q, el = document) => el.querySelector(q);
const $$ = (q, el = document) => [...el.querySelectorAll(q)];

// If launched with ?t=<token> (e.g. from the Hammerspoon popover), capture
// the token and use it as a Bearer header on every API call — WKWebView
// doesn't reliably retain cookies, so we don't depend on them.
const AUTH_TOKEN = (() => {
  try {
    const fromUrl = new URLSearchParams(location.search).get("t");
    if (fromUrl) {
      sessionStorage.setItem("ciu_token", fromUrl);
      return fromUrl;
    }
    return sessionStorage.getItem("ciu_token") || "";
  } catch {
    return "";
  }
})();

/* -------------------------------------------------------------------- */
/*  xterm lifecycle — one live Terminal bound to the selected instance   */
/* -------------------------------------------------------------------- */
const XTERM_DEBUG = new URLSearchParams(location.search).get("xdebug") === "1";
const XTERM_DEBUG_MAX_CHUNKS = 40;

// Make the control bytes/escape sequences human-readable so we can spot
// underlines (ESC[4m), 256-color bg (ESC[48;5;Nm), half-block glyphs (▀ ▄ █),
// and other suspects that could paint horizontal bars.
function formatAnsiForLog(bytes) {
  let s;
  try {
    s = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  } catch {
    s = String(bytes);
  }
  return s.replace(/\x1b/g, "\\e").replace(/[\x00-\x1f\x7f]/g, (c) =>
    c === "\n" ? "\\n\n" :
    c === "\r" ? "\\r" :
    c === "\t" ? "\\t" :
    `\\x${c.charCodeAt(0).toString(16).padStart(2, "0")}`
  );
}

// Pull out just the SGR, cursor-position, and high-interest escape codes
// from a chunk — far less noise than the full byte dump.
function summarizeAnsi(bytes) {
  let s;
  try { s = new TextDecoder("utf-8", { fatal: false }).decode(bytes); }
  catch { return ""; }
  const out = [];
  // SGR (color/attribute) codes
  const sgr = s.match(/\x1b\[[\d;]*m/g) || [];
  for (const c of sgr) {
    const params = c.slice(2, -1).split(";").filter(Boolean).map(Number);
    if (!params.length) { out.push("SGR[RESET]"); continue; }
    const notes = [];
    for (let i = 0; i < params.length; i++) {
      const p = params[i];
      if (p === 0) notes.push("RESET");
      else if (p === 1) notes.push("BOLD");
      else if (p === 2) notes.push("DIM");
      else if (p === 3) notes.push("ITALIC");
      else if (p === 4) notes.push("*UNDERLINE*");
      else if (p === 7) notes.push("*INVERSE*");
      else if (p === 9) notes.push("STRIKE");
      else if (p === 53) notes.push("*OVERLINE*");
      else if (p >= 30 && p <= 37) notes.push(`fg${p - 30}`);
      else if (p >= 40 && p <= 47) notes.push(`*bg${p - 40}*`);
      else if (p >= 90 && p <= 97) notes.push(`fg${p - 90}+bright`);
      else if (p >= 100 && p <= 107) notes.push(`*bg${p - 100}+bright*`);
      else if (p === 38 && params[i + 1] === 5) { notes.push(`fg256:${params[i + 2]}`); i += 2; }
      else if (p === 48 && params[i + 1] === 5) { notes.push(`*bg256:${params[i + 2]}*`); i += 2; }
      else if (p === 38 && params[i + 1] === 2) { notes.push(`fgRGB:${params[i + 2]},${params[i + 3]},${params[i + 4]}`); i += 4; }
      else if (p === 48 && params[i + 1] === 2) { notes.push(`*bgRGB:${params[i + 2]},${params[i + 3]},${params[i + 4]}*`); i += 4; }
      else notes.push(`p${p}`);
    }
    out.push(`SGR[${notes.join(",")}]`);
  }
  // Suspect characters that could paint horizontal bars
  const blocks = s.match(/[▀▄█▁▂▃▅▆▇─━═]/g) || [];
  if (blocks.length) out.push(`GLYPHS:${[...new Set(blocks)].join("")}×${blocks.length}`);
  return out.join(" ");
}

const xtermManager = (() => {
  // Retro-futuristic palette: deep grey base, orange + white accents.
  // Full 16-color ANSI override so Claude's chrome and content render
  // in the branded palette rather than xterm's stock colours.
  const THEME = {
    background: "#111111",
    foreground: "#d8d8d8",
    cursor: "#ff6b1a",
    cursorAccent: "#111111",
    selectionBackground: "rgba(255, 107, 26, 0.25)",
    black: "#111111",
    brightBlack: "#3a3a3a",
    red: "#ff4444",
    brightRed: "#ff6b6b",
    green: "#3ddc84",
    brightGreen: "#7eeca0",
    yellow: "#ffb347",
    brightYellow: "#ffd080",
    blue: "#5b9bf5",
    brightBlue: "#8cb8ff",
    magenta: "#bb86fc",
    brightMagenta: "#d4aaff",
    cyan: "#4dd0e1",
    brightCyan: "#80e0ee",
    white: "#b0b0c0",
    brightWhite: "#f0f0ff",
  };

  let term = null;
  let fit = null;
  let ws = null;
  let host = null;
  let sid = null;
  let resizeObs = null;
  let onResizeWin = null;
  let resizeDebounce = null;

  const wsUrl = (sessionId) => {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    const qs = AUTH_TOKEN ? `?t=${encodeURIComponent(AUTH_TOKEN)}` : "";
    return `${proto}//${location.host}/ws/instances/${sessionId}/terminal${qs}`;
  };

  const dispose = () => {
    if (resizeDebounce) {
      clearTimeout(resizeDebounce);
      resizeDebounce = null;
    }
    if (onResizeWin) {
      window.removeEventListener("resize", onResizeWin);
      onResizeWin = null;
    }
    if (resizeObs) {
      try { resizeObs.disconnect(); } catch {}
      resizeObs = null;
    }
    if (ws) {
      try { ws.close(); } catch {}
      ws = null;
    }
    if (term) {
      try { term.dispose(); } catch {}
      term = null;
    }
    fit = null;
    host = null;
    sid = null;
  };

  const fitNow = () => {
    if (!fit || !term) return;
    try { fit.fit(); } catch {}
  };

  const sendResize = () => {
    if (!ws || ws.readyState !== WebSocket.OPEN || !term) return;
    ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
  };

  const attach = (sessionId, hostEl) => {
    if (sid === sessionId && term && ws && ws.readyState <= WebSocket.OPEN) return;
    dispose();
    if (!hostEl || !window.Terminal) return;
    sid = sessionId;
    host = hostEl;
    host.innerHTML = "";
    host.classList.add("connecting");
    host.classList.remove("disconnected");

    term = new window.Terminal({
      fontFamily: `"SF Mono", Menlo, Monaco, "Cascadia Mono", Consolas, "Liberation Mono", monospace`,
      fontSize: 13,
      fontWeight: 400,
      fontWeightBold: 700,
      lineHeight: 1.2,
      letterSpacing: 0,
      cursorBlink: true,
      cursorStyle: "block",
      cursorWidth: 1,
      allowProposedApi: true,
      scrollback: 8000,
      convertEol: false,
      macOptionIsMeta: true,
      rightClickSelectsWord: true,
      drawBoldTextInBrightColors: false,
      minimumContrastRatio: 1,
      // customGlyphs: true (xterm default) — lets xterm render box-drawing
      // chars as pixel-aligned hairlines via its built-in atlas instead of
      // the font's anti-aliased glyph, which on Retina paints `─` as a
      // chunky fuzzy bar. These bars are Claude's real turn dividers; we
      // just need them rendered crisp.
      theme: THEME,
    });
    fit = new window.FitAddon.FitAddon();
    term.loadAddon(fit);
    if (window.WebLinksAddon) {
      try { term.loadAddon(new window.WebLinksAddon.WebLinksAddon()); } catch {}
    }
    term.open(host);

    host.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter" && ev.shiftKey && !ev.ctrlKey && !ev.metaKey && !ev.altKey) {
        ev.preventDefault();
        ev.stopPropagation();
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ type: "input", data: "\x1b[13;2u" }));
        }
      }
    }, true);

    // Pixel-aligned renderer — default DOM renderer smears row backgrounds
    // across fractional pixels, producing pale horizontal banding between
    // Claude's `─` divider rows. Try WebGL, fall back to Canvas, else DOM.
    const pickCtor = (root, name) => {
      if (!root) return null;
      if (typeof root === "function") return root;
      if (root[name] && typeof root[name] === "function") return root[name];
      return null;
    };
    const tryLoadRenderer = () => {
      const candidates = [
        { name: "webgl", Ctor: pickCtor(window.WebglAddon, "WebglAddon") },
        { name: "canvas", Ctor: pickCtor(window.CanvasAddon, "CanvasAddon") },
      ];
      for (const c of candidates) {
        if (!c.Ctor) continue;
        try {
          const addon = new c.Ctor();
          if (typeof addon.onContextLoss === "function") {
            addon.onContextLoss(() => { try { addon.dispose(); } catch {} });
          }
          term.loadAddon(addon);
          console.log("[xterm] renderer:", c.name);
          return c.name;
        } catch (e) {
          console.warn(`[xterm] ${c.name} renderer failed:`, e);
        }
      }
      console.log("[xterm] renderer: dom (fallback)");
      return "dom";
    };
    tryLoadRenderer();

    // initial fit before we touch the WS so the resize we send is accurate
    requestAnimationFrame(() => {
      fitNow();
      openSocket();
    });

    // Debounced so a burst of events (browser zoom fires ResizeObserver
    // several times per Cmd+/- press; each SIGWINCH causes Ink to emit
    // a full redraw, and back-to-back redraws stack instead of clearing
    // cleanly) collapses into one final fit + resize.
    const scheduleResize = () => {
      if (resizeDebounce) clearTimeout(resizeDebounce);
      resizeDebounce = setTimeout(() => {
        resizeDebounce = null;
        fitNow();
        sendResize();
      }, 150);
    };
    resizeObs = new ResizeObserver(scheduleResize);
    resizeObs.observe(host);
    onResizeWin = scheduleResize;
    window.addEventListener("resize", onResizeWin);

    term.onData((data) => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "input", data }));
      }
    });
    term.onResize(() => sendResize());
  };

  const openSocket = () => {
    if (!sid) return;
    const capturedSid = sid;
    ws = new WebSocket(wsUrl(sid));
    ws.binaryType = "arraybuffer";
    let dbgCount = 0;
    ws.onopen = () => {
      if (sid !== capturedSid) return;
      // Initial handshake: server waits for a resize before streaming so the
      // snapshot matches the xterm viewport.
      sendResize();
      if (host) host.classList.remove("connecting");
      if (XTERM_DEBUG) {
        console.log("[xterm rx] WS open — logging first", XTERM_DEBUG_MAX_CHUNKS, "chunks");
      }
    };
    ws.onmessage = (evt) => {
      if (!term || sid !== capturedSid) return;
      const data = evt.data instanceof ArrayBuffer ? new Uint8Array(evt.data) : evt.data;
      if (XTERM_DEBUG && dbgCount < XTERM_DEBUG_MAX_CHUNKS) {
        dbgCount++;
        const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
        const summary = summarizeAnsi(bytes);
        console.groupCollapsed(
          `[xterm rx #${dbgCount}] ${bytes.byteLength}B — ${summary.slice(0, 180) || "(no SGR/glyphs)"}`
        );
        console.log(formatAnsiForLog(bytes));
        console.groupEnd();
      }
      term.write(data);
    };
    ws.onclose = () => {
      if (host && sid === capturedSid) {
        host.classList.remove("connecting");
        host.classList.add("disconnected");
      }
    };
    ws.onerror = () => {
      if (host && sid === capturedSid) {
        host.classList.remove("connecting");
        host.classList.add("disconnected");
      }
    };
  };

  const writeText = (text) => {
    if (!ws || ws.readyState !== WebSocket.OPEN) return false;
    ws.send(JSON.stringify({ type: "input", data: text }));
    return true;
  };

  const focus = () => { if (term) term.focus(); };

  return { attach, detach: dispose, writeText, focus, fit: fitNow, get sid() { return sid; } };
})();

async function api(method, url, body) {
  const headers = {};
  if (body) headers["Content-Type"] = "application/json";
  if (AUTH_TOKEN) headers["Authorization"] = `Bearer ${AUTH_TOKEN}`;
  const res = await fetch(url, {
    method,
    headers,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.json();
}

function toast(msg, opts = {}) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.toggle("error", !!opts.error);
  t.classList.add("show");
  clearTimeout(t._timer);
  t._timer = setTimeout(() => t.classList.remove("show"), 2800);
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

function relTime(ts) {
  if (!ts) return "";
  const d = typeof ts === "string" ? Date.parse(ts) / 1000 : ts;
  const diff = Date.now() / 1000 - d;
  if (diff < 5) return "now";
  if (diff < 60) return `${Math.floor(diff)}s`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

function fmtTime(ts) {
  if (!ts) return "";
  const d = typeof ts === "string" ? new Date(ts) : new Date(ts * 1000);
  return d.toTimeString().slice(0, 8);
}

function truncCwd(cwd) {
  if (!cwd) return "—";
  const home = "/Users/";
  if (cwd.startsWith(home)) {
    const rest = cwd.slice(home.length);
    const i = rest.indexOf("/");
    if (i >= 0) return "~" + rest.slice(i);
  }
  return cwd;
}

const STATUS_GLYPH = {
  running: "▸",
  idle: "●",
  needs_input: "!",
  ended: "×",
};
const STATUS_TEXT = {
  running: "RUNNING",
  idle: "IDLE",
  needs_input: "NEEDS INPUT",
  ended: "ENDED",
};

/* List card */
function renderListCard(inst) {
  const tpl = $("#listCardTpl");
  const node = tpl.content.firstElementChild.cloneNode(true);
  node.dataset.sid = inst.session_id;
  updateListCard(node, inst);
  const isCompact = () => document.body.classList.contains("compact");
  node.addEventListener("click", () => {
    acknowledge(inst.session_id, inst.hook_timestamp);
    if (isCompact()) {
      // In compact tray mode: focus an existing dashboard tab on this sid,
      // or open a new one.
      api("POST", "/api/open-dashboard", { sid: inst.session_id }).catch(() => {});
      window.location.href = "ciu://close";
      return;
    }
    selectInstance(inst.session_id);
  });
  node.addEventListener("dblclick", (e) => {
    e.preventDefault();
    selectInstance(inst.session_id);
    setTimeout(() => xtermManager.focus(), 250);
  });
  wireDragAndDrop(node, inst.session_id);
  return node;
}

function updateListCard(node, inst) {
  const intensity = freshIntensity(inst);
  const isFresh = intensity > 0;
  // Toggle only the state-dependent classes so we don't nuke layout classes
  // (in-group / group-first / group-last / drag-* / merge-*) applied elsewhere.
  const statuses = ["running", "idle", "needs_input", "ended"];
  for (const s of statuses) node.classList.toggle(s, inst.status === s);
  node.classList.toggle("fresh", isFresh);
  node.classList.toggle("selected", inst.session_id === state.selectedSid);
  node.style.setProperty("--fresh", intensity.toFixed(3));
  const sprite = $(".lc-sprite", node);
  if (sprite && !sprite.dataset.seeded) {
    sprite.innerHTML = window.agentSprite ? window.agentSprite(inst.session_id) : "";
    sprite.dataset.seeded = "1";
  }
  $(".lc-status-glyph", node).textContent = STATUS_GLYPH[inst.status] || "○";
  $(".lc-status-text", node).textContent = STATUS_TEXT[inst.status] || inst.status;
  const ts = inst.hook_timestamp || inst.transcript?.last_timestamp;
  $(".lc-status-time", node).textContent = ts ? relTime(ts) : "";
  $(".lc-name", node).textContent = inst.name;
  $(".lc-cwd", node).textContent = truncCwd(inst.cwd);
  $(".lc-cwd", node).title = inst.cwd || "";
  $(".lc-notif", node).textContent =
    inst.status === "needs_input" && inst.notification_message ? inst.notification_message : "";
  const meta = $(".lc-meta", node);
  const mcpCount =
    (inst.mcps?.global?.length || 0) +
    (inst.mcps?.project?.length || 0) +
    (inst.mcps?.explicit?.length || 0);
  const subCount = inst.subagents ? inst.subagents.length : 0;
  const metaSig = [inst.group || "", mcpCount, inst.pid || 0, inst.status === "running" ? inst.last_tool || "" : "", subCount].join("|");
  if (meta.dataset.sig !== metaSig) {
    meta.dataset.sig = metaSig;
    meta.innerHTML = "";
    const mk = (cls, txt) => {
      const s = document.createElement("span");
      s.className = `lc-tag ${cls}`;
      s.textContent = txt;
      meta.append(s);
    };
    if (inst.group) mk("group", inst.group);
    if (mcpCount) mk("", `${mcpCount} MCP${mcpCount === 1 ? "" : "s"}`);
    if (inst.pid) mk("", `pid ${inst.pid}`);
    if (inst.status === "running" && inst.last_tool) mk("", inst.last_tool);
    if (subCount) mk("subagent", `${subCount} agent${subCount === 1 ? "" : "s"}`);
  }
}

function selectInstance(sid) {
  if (state.selectedSid === sid && !configEditor.mode) return;
  if (configEditor.mode) configEditor.close();
  const inst = state.instances.find((i) => i.session_id === sid);
  if (inst) acknowledge(sid, inst.hook_timestamp);
  state.selectedSid = sid;
  state.transcriptData = null;
  state.renderedSid = null;
  xtermManager.detach();
  renderList();
  $("#preview").innerHTML = `
    <div class="empty">
      <div class="empty-cursor">█</div>
      <h3>Loading…</h3>
    </div>`;
  loadTranscript();
}

/* List rendering (diff-aware) */
function filtered() {
  return state.instances.filter((i) => {
    if (!state.showEnded && !i.alive) return false;
    if (state.filter !== "all" && i.status !== state.filter) return false;
    if (state.groupFilter && i.group !== state.groupFilter) return false;
    if (state.query) {
      const q = state.query.toLowerCase();
      const hay = [
        i.name, i.cwd, i.group,
        ...(i.mcps?.global || []),
        ...(i.mcps?.project || []),
        ...(i.mcps?.explicit || []),
      ].filter(Boolean).join(" ").toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
}

function listSignature(items) {
  const freshBucket = Math.floor(Date.now() / 15000);
  return items
    .map((i) => [
      i.session_id, i.status, i.name, i.group || "", i.pid || 0,
      i.last_tool || "", i.hook_timestamp || 0, i.notification_message || "",
      freshIntensity(i) > 0 ? freshBucket : 0,
    ].join("|"))
    .join("#") + `§${state.selectedSid || ""}§${state.query}§${state.filter}§${state.groupFilter || ""}§${state.showEnded}§${[...state.collapsedGroups].sort().join(",")}`;
}

function renderList() {
  const list = $("#list");
  const items = filtered();
  const sig = listSignature(items);
  if (sig === state.lastListSig) {
    $$(".list-card", list).forEach((node) => {
      const sid = node.dataset.sid;
      const inst = items.find((i) => i.session_id === sid);
      if (inst) updateListCard(node, inst);
    });
    return;
  }
  state.lastListSig = sig;
  list.innerHTML = "";
  if (!items.length) {
    const empty = document.createElement("div");
    empty.style.cssText = "color:var(--muted);padding:40px 20px;text-align:center;font-size:12px";
    empty.textContent = "No matching instances";
    list.append(empty);
    return;
  }
  const buckets = new Map();
  for (const i of items) {
    const key = i.group || "Ungrouped";
    if (!buckets.has(key)) buckets.set(key, []);
    buckets.get(key).push(i);
  }
  const order = [...buckets.keys()].sort((a, b) => {
    if (a === "Ungrouped") return 1;
    if (b === "Ungrouped") return -1;
    return a.localeCompare(b);
  });
  const needsHeaders = order.length > 1 || order[0] !== "Ungrouped";
  for (const key of order) {
    const groupItems = buckets.get(key);
    const collapsed = state.collapsedGroups.has(key);
    if (needsHeaders) {
      const hdr = document.createElement("div");
      hdr.className = "list-group-label";
      if (collapsed) hdr.classList.add("collapsed");
      hdr.dataset.group = key;
      const gc = key !== "Ungrouped" ? groupColor(key) : "var(--muted)";
      hdr.style.setProperty("--group-color", gc);
      const statusDots = groupItems.map((i) => `<span class="dot ${i.status}"></span>`).join("");
      hdr.innerHTML = `<span class="lg-chevron">${collapsed ? "▸" : "▾"}</span><span class="lg-name">${escapeHtml(key)}</span><span class="g-count">${groupItems.length}</span>${collapsed ? `<span class="lg-dots">${statusDots}</span>` : ""}`;
      const chevron = $(".lg-chevron", hdr);
      if (chevron) chevron.addEventListener("click", (e) => {
        e.stopPropagation();
        if (state.collapsedGroups.has(key)) state.collapsedGroups.delete(key);
        else state.collapsedGroups.add(key);
        state.lastListSig = "";
        renderList();
      });
      if (key !== "Ungrouped") {
        hdr.title = "Double-click to rename";
        hdr.addEventListener("dblclick", (e) => {
          e.preventDefault();
          startInlineRename(hdr, key);
        });
        wireGroupHeaderDrop(hdr, key);
      }
      list.append(hdr);
    }
    if (!collapsed) {
      const grouped = key !== "Ungrouped";
      groupItems.forEach((inst, idx) => {
        const card = renderListCard(inst);
        if (grouped) {
          card.classList.add("in-group");
          if (idx === 0) card.classList.add("group-first");
          if (idx === groupItems.length - 1) card.classList.add("group-last");
        }
        list.append(card);
      });
    }
  }
}

/* Preview */
async function loadTranscript() {
  if (!state.selectedSid) return;
  try {
    const data = await api(
      "GET",
      `/api/instances/${state.selectedSid}/transcript?limit=120`,
    );
    state.transcriptData = data;
    renderPreview();
  } catch (e) {
    console.error(e);
  }
}

function ensurePreviewShell() {
  const root = $("#preview");
  if ($(".preview-root", root)) return $(".preview-root", root);
  const tpl = $("#previewTpl");
  const shell = tpl.content.firstElementChild.cloneNode(true);
  root.innerHTML = "";
  root.append(shell);
  wirePreviewActions(shell);
  return shell;
}

function renderPreview() {
  if (!state.selectedSid || !state.transcriptData) return;
  const { session } = state.transcriptData;
  const shell = ensurePreviewShell();

  const sidChanged = state.renderedSid !== session.session_id;
  if (sidChanged) {
    state.renderedSid = session.session_id;
    const hostEl = $("#xtermHost", shell);
    if (hostEl) xtermManager.attach(session.session_id, hostEl);
  }

  shell.className = `preview-root ${session.status}`;
  const headSprite = $(".term-title-sprite", shell);
  if (headSprite && headSprite.dataset.sid !== session.session_id) {
    headSprite.innerHTML = window.agentSprite ? window.agentSprite(session.session_id) : "";
    headSprite.dataset.sid = session.session_id;
  }
  $(".term-title-name", shell).textContent = session.name;
  $(".term-title-status", shell).textContent = STATUS_TEXT[session.status] || session.status;
  $(".ph-cwd", shell).textContent = truncCwd(session.cwd);
  $(".ph-pid", shell).textContent = session.pid ? `pid ${session.pid}` : "no pid";
  $(".ph-sid", shell).textContent = session.session_id.slice(0, 8);
  const groupChip = $(".ph-group-chip", shell);
  groupChip.textContent = session.group || "ungrouped";
  groupChip.classList.toggle("accent", !!session.group);
  const mcpCount =
    (session.mcps?.global?.length || 0) +
    (session.mcps?.project?.length || 0) +
    (session.mcps?.explicit?.length || 0);
  $(".ph-mcp-chip", shell).textContent = `${mcpCount} MCP${mcpCount === 1 ? "" : "s"}`;

  const banner = $(".notif-banner", shell);
  if (session.notification_message && session.status === "needs_input") {
    banner.classList.add("show");
    banner.innerHTML = "";
    const msg = document.createElement("span");
    msg.className = "nb-msg";
    msg.textContent = session.notification_message;
    banner.append(msg);
  } else {
    banner.classList.remove("show");
    banner.innerHTML = "";
  }

  renderSummary(shell, session.summary);
  renderSubagents(shell, session.subagents || []);

  // Stop button only enabled for alive sessions
  const stopBtn = $(".ph-stop", shell);
  if (stopBtn) stopBtn.disabled = !session.alive;

  // Auto-focus the terminal on fresh selection
  if (sidChanged && session.alive) {
    setTimeout(() => xtermManager.focus(), 60);
  }
}

function renderSubagents(shell, agents) {
  const strip = $(".subagent-strip", shell);
  if (!strip) return;
  if (!agents.length) {
    strip.classList.remove("show");
    return;
  }
  const sig = agents.map((a) => a.agent_id).join(",");
  if (strip.dataset.sig === sig) return;
  strip.dataset.sig = sig;
  strip.classList.add("show");
  const wasOpen = strip.classList.contains("open");
  strip.innerHTML = "";
  const toggle = document.createElement("button");
  toggle.className = "sa-toggle";
  toggle.innerHTML = `<span class="sa-arrow">${wasOpen ? "▾" : "▸"}</span><span class="sa-label">${agents.length} subagent${agents.length === 1 ? "" : "s"}</span>`;
  toggle.addEventListener("click", () => {
    strip.classList.toggle("open");
    $(".sa-arrow", toggle).textContent = strip.classList.contains("open") ? "▾" : "▸";
  });
  strip.append(toggle);
  if (wasOpen) strip.classList.add("open");
  const list = document.createElement("div");
  list.className = "sa-list";
  for (const a of agents) {
    const row = document.createElement("div");
    row.className = "sa-row";
    const age = a.mtime ? relTime(a.mtime) : "";
    row.innerHTML = `<span class="sa-dot"></span><span class="sa-text"></span><span class="sa-age">${age}</span>`;
    $(".sa-text", row).textContent = a.label;
    list.append(row);
  }
  strip.append(list);
}

function renderSummary(shell, summary) {
  const strip = $(".summary-strip", shell);
  if (!strip) return;
  const paragraph = summary && summary.paragraph;
  if (!paragraph) {
    strip.classList.remove("show");
    return;
  }
  strip.classList.add("show");
  $(".summary-para-text", strip).textContent = paragraph;
}

function wirePreviewActions(shell) {
  const sess = () => state.transcriptData?.session;

  $(".ph-edit", shell).addEventListener("click", () => {
    const s = sess(); if (!s) return;
    showEditModal(s);
  });

  $(".ph-open-term", shell).addEventListener("click", async () => {
    const s = sess(); if (!s) return;
    try {
      await api("POST", `/api/instances/${s.session_id}/open-terminal`);
      toast("Opened in terminal");
    } catch (e) {
      toast(`Failed: ${e.message}`, { error: true });
    }
  });

  $(".ph-stop", shell).addEventListener("click", async () => {
    const s = sess(); if (!s) return;
    const label = s.custom_name || s.name;
    if (!confirm(`Stop "${label}"? This kills the process.`)) return;
    try {
      await api("POST", `/api/instances/${s.session_id}/kill`);
      toast("Stopped");
      refresh();
    } catch (e) {
      toast(`Stop failed: ${e.message}`, { error: true });
    }
  });

  const more = $(".ph-more", shell);
  const menu = $(".menu", shell);
  more.addEventListener("click", (e) => {
    e.stopPropagation();
    menu.classList.toggle("open");
  });
  document.addEventListener("click", () => menu.classList.remove("open"));
  $$("button[data-act]", menu).forEach((btn) =>
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      menu.classList.remove("open");
      const s = sess(); if (!s) return;
      const act = btn.dataset.act;
      if (act === "focus") {
        xtermManager.focus();
      } else if (act === "sigint") {
        await api("POST", `/api/instances/${s.session_id}/signal`, { signal: "INT" });
        toast("SIGINT sent");
      } else if (act === "forget") {
        xtermManager.detach();
        await api("DELETE", `/api/instances/${s.session_id}`);
        state.selectedSid = null;
        state.transcriptData = null;
        state.renderedSid = null;
        $("#preview").innerHTML = `
          <div class="empty">
            <div class="empty-cursor">█</div>
            <h3>No instance selected</h3>
          </div>`;
        toast("Forgotten");
        refresh();
      }
    })
  );

  // Clicking the terminal area focuses it — convenience for laptops where
  // xterm's own focus handling can drop after a DOM reshuffle.
  const host = $("#xtermHost", shell);
  if (host) {
    host.addEventListener("mousedown", () => setTimeout(() => xtermManager.focus(), 0));
  }
}

/* Drag and drop — custom pointer-based, works in WKWebView */
const HOLD_MS = 600;
const DRAG_THRESHOLD = 6;
const MERGE_BAND = 0.5; // central 50% of a card's height = merge zone

function wireDragAndDrop(node, sid) {
  node.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    const innerInteractive = e.target.closest("button:not(.list-card), input, .lg-rename-input");
    if (innerInteractive && innerInteractive !== node) return;

    const startX = e.clientX;
    const startY = e.clientY;
    const rect = node.getBoundingClientRect();
    const offX = startX - rect.left;
    const offY = startY - rect.top;

    let started = false;
    let suppressClick = false;
    let clone = null;
    let holdTimer = null;
    let dropMode = null; // { kind: "merge", target } | { kind: "insert", before, after, group }
    let insertLine = null;

    const clearMarks = () => {
      document.querySelectorAll(".merge-target, .merge-ready").forEach((n) =>
        n.classList.remove("merge-target", "merge-ready"));
    };

    const ensureLine = () => {
      if (insertLine) return insertLine;
      insertLine = document.createElement("div");
      insertLine.className = "drag-insert-line";
      document.body.append(insertLine);
      return insertLine;
    };

    const removeLine = () => {
      if (insertLine) { insertLine.remove(); insertLine = null; }
    };

    const setInsertLine = (yScreen, leftScreen, width) => {
      const ln = ensureLine();
      ln.style.transform = `translate(${leftScreen}px, ${yScreen - 1}px)`;
      ln.style.width = `${width}px`;
    };

    const computeDropMode = (ev) => {
      clone.style.display = "none";
      const elBelow = document.elementFromPoint(ev.clientX, ev.clientY);
      clone.style.display = "";
      if (!elBelow) return null;

      const overCard = elBelow.closest(".list-card");
      const overHeader = elBelow.closest(".list-group-label");

      if (overCard && overCard !== node) {
        const r = overCard.getBoundingClientRect();
        const relY = (ev.clientY - r.top) / r.height;
        const lo = (1 - MERGE_BAND) / 2;
        const hi = 1 - lo;
        if (relY >= lo && relY <= hi) {
          return { kind: "merge", target: overCard };
        }
        // Insertion above or below this card
        const insertBefore = relY < lo;
        const cards = Array.from(document.querySelectorAll(".list-card"));
        const idx = cards.indexOf(overCard);
        const before = insertBefore ? overCard : cards[idx + 1] || null;
        const after = insertBefore ? cards[idx - 1] || null : overCard;
        const group = inferGroupFor(overCard);
        return {
          kind: "insert",
          y: insertBefore ? r.top : r.bottom,
          left: r.left,
          width: r.width,
          before, after, group,
        };
      }
      if (overHeader) {
        const r = overHeader.getBoundingClientRect();
        return { kind: "merge", target: overHeader };
      }
      return null;
    };

    const inferGroupFor = (cardEl) => {
      // Walk back to the nearest preceding .list-group-label
      let cur = cardEl.previousElementSibling;
      while (cur) {
        if (cur.classList.contains("list-group-label")) return cur.dataset.group;
        cur = cur.previousElementSibling;
      }
      return "Ungrouped";
    };

    const onMove = (ev) => {
      const dx = ev.clientX - startX;
      const dy = ev.clientY - startY;
      if (!started && Math.hypot(dx, dy) < DRAG_THRESHOLD) return;

      if (!started) {
        started = true;
        suppressClick = true;
        node.classList.add("dragging");
        clone = node.cloneNode(true);
        clone.classList.add("drag-clone");
        clone.style.position = "fixed";
        clone.style.top = "0";
        clone.style.left = "0";
        clone.style.width = rect.width + "px";
        clone.style.zIndex = "10000";
        clone.style.pointerEvents = "none";
        clone.style.transform = `translate(${ev.clientX - offX}px, ${ev.clientY - offY}px)`;
        document.body.append(clone);
        document.body.style.cursor = "grabbing";
        document.body.style.userSelect = "none";
      }

      clone.style.transform = `translate(${ev.clientX - offX}px, ${ev.clientY - offY}px)`;

      const newMode = computeDropMode(ev);

      const sameTarget = newMode && dropMode && newMode.kind === dropMode.kind && (
        (newMode.kind === "merge" && newMode.target === dropMode.target) ||
        (newMode.kind === "insert" && newMode.before === dropMode.before && newMode.after === dropMode.after)
      );
      if (!sameTarget) {
        clearTimeout(holdTimer);
        clearMarks();
        if (!newMode || newMode.kind !== "insert") removeLine();
        dropMode = newMode;
        if (newMode && newMode.kind === "merge") {
          newMode.target.classList.add("merge-target");
          holdTimer = setTimeout(() => newMode.target.classList.add("merge-ready"), HOLD_MS);
        } else if (newMode && newMode.kind === "insert") {
          setInsertLine(newMode.y, newMode.left, newMode.width);
        }
      } else if (newMode && newMode.kind === "insert") {
        setInsertLine(newMode.y, newMode.left, newMode.width);
      }
    };

    const onUp = async () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      clearTimeout(holdTimer);
      if (!started) return;
      node.classList.remove("dragging");
      if (clone) clone.remove();
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      removeLine();
      const finalMode = dropMode;
      clearMarks();
      if (finalMode) {
        if (finalMode.kind === "merge") {
          if (finalMode.target.classList.contains("list-card")) {
            await mergeIntoGroup(sid, finalMode.target.dataset.sid);
          } else if (finalMode.target.classList.contains("list-group-label")) {
            await addSessionToGroup(sid, finalMode.target.dataset.group);
          }
        } else if (finalMode.kind === "insert") {
          const targetGroup = finalMode.group;
          if (targetGroup === "Ungrouped") {
            await removeFromAnyGroup(sid);
          } else {
            await addSessionToGroup(sid, targetGroup);
          }
        }
      }
      if (suppressClick) {
        const blocker = (ev) => {
          ev.stopPropagation();
          ev.preventDefault();
          window.removeEventListener("click", blocker, true);
        };
        window.addEventListener("click", blocker, true);
      }
    };

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  });
}

async function removeFromAnyGroup(sid) {
  const groups = await api("GET", "/api/groups");
  let touched = false;
  for (const g of Object.keys(groups)) {
    const before = groups[g].length;
    groups[g] = groups[g].filter((s) => s !== sid);
    if (groups[g].length !== before) touched = true;
    if (!groups[g].length) delete groups[g];
  }
  if (touched) {
    await api("PUT", "/api/groups", groups);
    refresh();
  }
}

function wireGroupHeaderDrop(_headerEl, _groupName) {
  // Drop targeting handled inside the unified mousemove logic
}

async function mergeIntoGroup(sourceSid, targetSid) {
  const groups = await api("GET", "/api/groups");
  const targetGroup = Object.entries(groups).find(([, ids]) => ids.includes(targetSid))?.[0];
  const sourceGroup = Object.entries(groups).find(([, ids]) => ids.includes(sourceSid))?.[0];
  if (targetGroup && targetGroup === sourceGroup) return;

  if (sourceGroup) {
    groups[sourceGroup] = groups[sourceGroup].filter((s) => s !== sourceSid);
    if (!groups[sourceGroup].length) delete groups[sourceGroup];
  }

  if (targetGroup) {
    if (!groups[targetGroup].includes(sourceSid)) groups[targetGroup].push(sourceSid);
    await api("PUT", "/api/groups", groups);
    refresh();
    return;
  }

  const baseName = "Group";
  let name = baseName;
  let n = 1;
  while (groups[name]) { n += 1; name = `${baseName} ${n}`; }
  groups[name] = [sourceSid, targetSid];
  await api("PUT", "/api/groups", groups);
  await refresh();
  flashRenameGroup(name);
}

async function addSessionToGroup(sid, groupName) {
  const groups = await api("GET", "/api/groups");
  for (const g of Object.keys(groups)) {
    groups[g] = groups[g].filter((s) => s !== sid);
    if (!groups[g].length) delete groups[g];
  }
  groups[groupName] = groups[groupName] || [];
  if (!groups[groupName].includes(sid)) groups[groupName].push(sid);
  await api("PUT", "/api/groups", groups);
  refresh();
}

async function renameGroup(oldName, newName) {
  newName = (newName || "").trim();
  if (!newName || newName === oldName) return;
  const groups = await api("GET", "/api/groups");
  if (groups[newName]) {
    groups[newName] = [...new Set([...(groups[newName] || []), ...(groups[oldName] || [])])];
  } else {
    groups[newName] = groups[oldName] || [];
  }
  delete groups[oldName];
  await api("PUT", "/api/groups", groups);
  refresh();
}

function flashRenameGroup(name) {
  setTimeout(() => {
    const hdr = [...document.querySelectorAll(".list-group-label")]
      .find((h) => h.dataset.group === name);
    if (hdr) startInlineRename(hdr, name);
  }, 60);
}

function startInlineRename(headerEl, currentName) {
  const labelSpan = headerEl.querySelector(".lg-name");
  if (!labelSpan || headerEl.querySelector(".lg-rename-input")) return;
  const input = document.createElement("input");
  input.type = "text";
  input.value = currentName;
  input.className = "lg-rename-input";
  input.maxLength = 40;
  labelSpan.replaceWith(input);
  input.focus();
  input.select();
  const finish = (commit) => {
    const v = input.value;
    if (commit) renameGroup(currentName, v);
    else input.replaceWith(labelSpan);
  };
  input.addEventListener("blur", () => finish(true));
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); finish(true); }
    if (e.key === "Escape") { e.preventDefault(); finish(false); }
  });
}

/* Filter dropdown */
function renderNav() {
  const filterBtn = $("#filterBtn");
  const filterMenu = $("#filterMenu");
  if (!filterBtn || !filterMenu) return;
  const labels = { all: "All", needs_input: "Needs input", running: "Running", idle: "Idle", ended: "Ended" };
  $(".filter-label", filterBtn).textContent = labels[state.filter] || "All";
  filterBtn.classList.toggle("active", state.filter !== "all" || state.groupFilter);
  const dot = $(".filter-dot", filterBtn);
  if (dot) {
    dot.className = "filter-dot";
    if (state.filter !== "all") dot.classList.add(state.filter);
  }
  $$("[data-filter]", filterMenu).forEach((btn) => {
    btn.classList.toggle("active", state.filter === btn.dataset.filter && !state.groupFilter);
  });
}

const GROUP_COLORS = [
  "#ff6b6b", "#ffa94d", "#ffd43b", "#69db7c", "#38d9a9",
  "#4dabf7", "#748ffc", "#b197fc", "#e599f7", "#f06595",
  "#ff922b", "#51cf66", "#3bc9db", "#5c7cfa", "#cc5de8",
];
function groupColor(name) {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = ((h << 5) - h + name.charCodeAt(i)) | 0;
  return GROUP_COLORS[Math.abs(h) % GROUP_COLORS.length];
}

function renderGroupNav() {
  const groupNav = $("#groupNav");
  const groups = [...new Set(state.instances.map((i) => i.group).filter(Boolean))].sort();
  const sig = groups.join("|") + `§${state.groupFilter || ""}` + `§${groups.map((g) => state.instances.filter((i) => i.group === g).length).join(",")}`;
  if (groupNav._sig === sig) return;
  groupNav._sig = sig;
  groupNav.innerHTML = "";
  if (!groups.length) {
    const empty = document.createElement("div");
    empty.textContent = "No groups · click ＋";
    empty.style.cssText = "font-size:11px;color:var(--muted);padding:4px 10px";
    groupNav.append(empty);
    return;
  }
  for (const g of groups) {
    const row = document.createElement("button");
    row.className = "group-nav-item" + (state.groupFilter === g ? " active" : "");
    const gc = groupColor(g);
    row.innerHTML = `
      <span class="dot" style="background:${gc}"></span>
      <span class="nav-label">${escapeHtml(g)}</span>
      <span class="count">${state.instances.filter((i) => i.group === g).length}</span>
      <button class="group-del" title="Delete group">×</button>`;
    row.addEventListener("click", (e) => {
      if (e.target.closest(".group-del")) return;
      state.groupFilter = state.groupFilter === g ? null : g;
      renderList();
      renderGroupNav();
      renderNav();
    });
    $(".group-del", row).addEventListener("click", async (e) => {
      e.stopPropagation();
      if (!confirm(`Delete group "${g}"?`)) return;
      const data = await api("GET", "/api/groups");
      delete data[g];
      await api("PUT", "/api/groups", data);
      if (state.groupFilter === g) state.groupFilter = null;
      refresh();
    });
    groupNav.append(row);
  }
}

/* Modals */
function showModal({ title, value, placeholder, suggestions, onSubmit }) {
  const bd = document.createElement("div");
  bd.className = "modal-backdrop";
  bd.innerHTML = `
    <div class="modal">
      <h4>${escapeHtml(title)}</h4>
      <input type="text" value="${escapeHtml(value || "")}" placeholder="${escapeHtml(placeholder || "")}" />
      <datalist id="modalSuggest">${(suggestions || []).map((s) => `<option value="${escapeHtml(s)}"></option>`).join("")}</datalist>
      <div class="modal-actions">
        <button class="btn-ghost cancel">Cancel</button>
        <button class="btn ok">Save</button>
      </div>
    </div>`;
  document.body.append(bd);
  const inp = $("input", bd);
  inp.setAttribute("list", "modalSuggest");
  inp.focus();
  inp.select();
  const close = () => bd.remove();
  const submit = async () => { close(); onSubmit(inp.value.trim()); };
  $(".cancel", bd).addEventListener("click", close);
  $(".ok", bd).addEventListener("click", submit);
  bd.addEventListener("click", (e) => { if (e.target === bd) close(); });
  inp.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submit();
    if (e.key === "Escape") close();
  });
}

async function showEditModal(session) {
  const groups = [...new Set(state.instances.map((i) => i.group).filter(Boolean))];
  const alive = !!session.alive;

  const bd = document.createElement("div");
  bd.className = "modal-backdrop edit-modal";
  bd.innerHTML = `
    <div class="modal-card">
      <h2>Edit instance</h2>
      <p class="modal-sub">Name &amp; group save instantly. Changing <b>directory</b> or <b>MCPs</b> requires a restart.</p>

      <label class="modal-label">Name</label>
      <input class="modal-input edit-name" placeholder="Display name" />

      <label class="modal-label">Group</label>
      <input class="modal-input edit-group" list="editGroupList" placeholder="Group (blank = ungrouped)" />
      <datalist id="editGroupList">${groups.map((g) => `<option value="${escapeHtml(g)}"></option>`).join("")}</datalist>

      <label class="modal-label">Working directory</label>
      <input class="modal-input edit-cwd" />

      <label class="modal-label">MCP source</label>
      <input class="modal-input edit-mcp-source" />
      <div class="edit-mcp-picker mcp-picker">
        <div class="mcp-picker-empty">Loading…</div>
      </div>

      <div class="modal-actions">
        <button class="modal-btn cancel">Close</button>
        <button class="modal-btn save-only">Save name/group</button>
        <button class="modal-btn primary restart" ${alive ? "" : "disabled"}>Restart with new config</button>
      </div>
    </div>`;
  document.body.append(bd);

  const nameI = $(".edit-name", bd);
  const groupI = $(".edit-group", bd);
  const cwdI = $(".edit-cwd", bd);
  const srcI = $(".edit-mcp-source", bd);
  const picker = $(".edit-mcp-picker", bd);

  nameI.value = session.custom_name || session.name || "";
  groupI.value = session.group || "";
  cwdI.value = session.cwd || "";
  const initialSrc = localStorage.getItem("ciu_last_mcp_source") || "~/.claude.json";
  srcI.value = initialSrc;

  let useMcps = null;

  const renderCheckboxes = (names, currentlyEnabled = null) => {
    picker.innerHTML = "";
    if (!names || !names.length) {
      picker.innerHTML = `<div class="mcp-picker-empty">No MCPs in this file</div>`;
      useMcps = () => [];
      return;
    }
    for (const name of names) {
      const row = document.createElement("label");
      row.className = "mcp-row";
      const checked = currentlyEnabled ? currentlyEnabled.includes(name) : true;
      row.innerHTML = `<input type="checkbox" value="${escapeHtml(name)}" ${checked ? "checked" : ""} /><span class="mcp-name">${escapeHtml(name)}</span>`;
      picker.append(row);
    }
    const quick = document.createElement("div");
    quick.className = "mcp-quick";
    quick.innerHTML = `<button type="button" data-act="all">all</button><button type="button" data-act="none">none</button>`;
    picker.append(quick);
    quick.addEventListener("click", (e) => {
      const act = e.target.dataset.act;
      if (!act) return;
      for (const cb of $$('input[type=checkbox]', picker)) cb.checked = act === "all";
    });
    useMcps = () => $$("input[type=checkbox]:checked", picker).map((c) => c.value);
  };

  const loadMcps = async (path) => {
    try {
      const res = await api("POST", "/api/mcp-list", { path });
      if (!res.exists) {
        picker.innerHTML = `<div class="mcp-picker-empty">File not found: <code>${escapeHtml(res.path)}</code></div>`;
        useMcps = () => [];
        return;
      }
      const enabled = [
        ...(session.mcps?.global || []),
        ...(session.mcps?.project || []),
        ...(session.mcps?.explicit || []),
      ];
      renderCheckboxes(res.mcps, enabled);
    } catch (e) {
      picker.innerHTML = `<div class="mcp-picker-empty">Failed: ${escapeHtml(e.message)}</div>`;
    }
  };
  loadMcps(initialSrc);
  let srcTimer = null;
  srcI.addEventListener("input", () => {
    clearTimeout(srcTimer);
    srcTimer = setTimeout(() => loadMcps(srcI.value.trim()), 350);
  });

  const close = () => bd.remove();
  $(".cancel", bd).addEventListener("click", close);
  bd.addEventListener("click", (e) => { if (e.target === bd) close(); });
  bd.addEventListener("keydown", (e) => { if (e.key === "Escape") close(); });

  $(".save-only", bd).addEventListener("click", async () => {
    try {
      await api("PUT", `/api/instances/${session.session_id}/name`, { name: nameI.value.trim() });
      await api("PUT", `/api/instances/${session.session_id}/group`, { group: groupI.value.trim() || null });
      toast("Saved");
      close();
      refresh();
    } catch (e) {
      toast(`Save failed: ${e.message}`, { error: true });
    }
  });

  const restartBtn = $(".restart", bd);
  if (restartBtn) {
    restartBtn.addEventListener("click", async () => {
      const newCwd = cwdI.value.trim();
      if (!newCwd) { cwdI.focus(); return; }
      const cwdChanged = newCwd !== (session.cwd || "");
      const resumeNote = cwdChanged ? "Fresh session (CWD changed)." : "Conversation will be continued.";
      if (!confirm(`Restart with new config?\n${resumeNote}`)) return;
      try {
        const oldSessionId = session.session_id;
        const newName = nameI.value.trim();
        const newGroup = groupI.value.trim() || null;
        await api("PUT", `/api/instances/${oldSessionId}/name`, { name: newName });
        await api("PUT", `/api/instances/${oldSessionId}/group`, { group: newGroup });
        await api("POST", `/api/instances/${oldSessionId}/kill`).catch(() => {});
        await new Promise((r) => setTimeout(r, 600));
        const cmd = cwdChanged ? "claude" : "claude --continue";
        const payload = {
          cwd: newCwd,
          command: cmd,
          name: newName || undefined,
          group: newGroup || undefined,
        };
        if (typeof useMcps === "function") {
          payload.mcps = useMcps();
          payload.mcp_source = srcI.value.trim();
        }
        const res = await api("POST", "/api/instances/new", payload);
        close();
        toast("Restarting…");
        const deadline = Date.now() + 12000;
        const tick = async () => {
          await refresh();
          const match = state.instances.find((i) => i.our_sid === res.our_sid);
          if (match) { selectInstance(match.session_id); toast("Restarted"); return; }
          if (Date.now() < deadline) { setTimeout(tick, 400); return; }
          toast("Instance spawned but not yet visible — check the terminal", { error: true });
        };
        tick();
      } catch (e) {
        toast(`Restart failed: ${e.message}`, { error: true });
      }
    });
  }
}

function promptNewGroup() {
  const alive = state.instances.filter((i) => i.alive);
  const bd = document.createElement("div");
  bd.className = "modal-backdrop";
  bd.innerHTML = `
    <div class="modal" style="width:440px">
      <h4>New group</h4>
      <input id="ng-name" type="text" placeholder="Group name (e.g. k8s, grafana)" />
      <div style="font-size:11px;color:var(--muted);text-transform:uppercase;letter-spacing:0.09em;font-weight:600;margin-bottom:6px;">Add instances</div>
      <div class="instance-picker" id="ng-picker"></div>
      <div class="modal-actions">
        <button class="btn-ghost cancel">Cancel</button>
        <button class="btn ok">Create</button>
      </div>
    </div>`;
  document.body.append(bd);
  const picker = $("#ng-picker", bd);
  for (const i of alive) {
    const row = document.createElement("label");
    row.className = "pick-row";
    row.innerHTML = `
      <input type="checkbox" value="${i.session_id}" />
      <span class="dot ${i.status}"></span>
      <span class="pick-name">${escapeHtml(i.name)}</span>
      <span class="pick-cwd">${escapeHtml(truncCwd(i.cwd))}</span>`;
    picker.append(row);
  }
  const nameInput = $("#ng-name", bd);
  nameInput.focus();
  const close = () => bd.remove();
  const submit = async () => {
    const name = nameInput.value.trim();
    if (!name) { nameInput.focus(); return; }
    const ids = $$("input[type=checkbox]:checked", picker).map((c) => c.value);
    const data = await api("GET", "/api/groups");
    data[name] = [...new Set([...(data[name] || []), ...ids])];
    for (const other of Object.keys(data)) {
      if (other === name) continue;
      data[other] = data[other].filter((s) => !ids.includes(s));
      if (!data[other].length) delete data[other];
    }
    await api("PUT", "/api/groups", data);
    close();
    toast(`Created "${name}" with ${ids.length} instance${ids.length === 1 ? "" : "s"}`);
    refresh();
  };
  $(".cancel", bd).addEventListener("click", close);
  $(".ok", bd).addEventListener("click", submit);
  bd.addEventListener("click", (e) => { if (e.target === bd) close(); });
  bd.addEventListener("keydown", (e) => {
    if (e.key === "Escape") close();
    if (e.key === "Enter" && e.target === nameInput) submit();
  });
}

/* Refresh */
let _refreshing = false;
let _pendingSidFromUrl = (() => {
  try {
    return new URLSearchParams(location.search).get("sid") || null;
  } catch {
    return null;
  }
})();
async function refresh() {
  if (_refreshing) return;
  _refreshing = true;
  try {
    const resp = await api("GET", "/api/instances");
    const { instances, served_at, pending_focus } = resp;
    state.instances = instances;
    $("#updated").textContent = `updated ${new Date(served_at * 1000).toLocaleTimeString()}`;
    renderNav();
    renderGroupNav();
    renderList();
    if (_pendingSidFromUrl) {
      const match = instances.find((i) => i.session_id === _pendingSidFromUrl);
      if (match) selectInstance(match.session_id);
      _pendingSidFromUrl = null;
    }
    if (pending_focus) {
      const match = instances.find((i) => i.session_id === pending_focus);
      if (match) {
        selectInstance(match.session_id);
        api("DELETE", "/api/pending-focus").catch(() => {});
      }
    }
    if (state.selectedSid) await loadTranscript();
  } catch (e) {
    console.error(e);
  } finally {
    _refreshing = false;
  }
}

/* ====================================================================== */
/*  Configuration editor                                                  */
/* ====================================================================== */
const configEditor = (() => {
  let mode = null; // "mcp" | "skills" | null
  let items = [];
  let activeItem = null;
  let dirty = false;

  const previewCol = () => $("#previewCol") || $("#preview");

  function open(configMode) {
    mode = configMode;
    activeItem = null;
    dirty = false;
    $$(".config-nav").forEach((b) => b.classList.toggle("active", b.dataset.config === mode));
    xtermManager.detach();
    state.selectedSid = null;
    state.lastListSig = "";
    renderList();
    loadFileList();
  }

  function close() {
    mode = null;
    activeItem = null;
    dirty = false;
    $$(".config-nav").forEach((b) => b.classList.remove("active"));
    const root = previewCol();
    root.innerHTML = `<div id="preview"><div class="empty"><div class="empty-cursor">█</div><h3>No instance selected</h3><p>Pick one on the left or click <b>＋</b> to start one.</p></div></div>`;
  }

  function renderShell() {
    const root = previewCol();
    const tpl = $("#configEditorTpl");
    root.innerHTML = "";
    const shell = tpl.content.firstElementChild.cloneNode(true);
    root.append(shell);

    const icons = { mcp: "⚙", skills: "⌘", claudemd: "📋" };
    const titles = { mcp: "MCP Servers", skills: "Skills", claudemd: "CLAUDE.md" };
    $(".config-editor-icon", shell).textContent = icons[mode] || "⚙";
    $(".config-editor-path", shell).textContent = titles[mode] || mode;

    $(".ce-save", shell).addEventListener("click", save);
    $(".ce-close", shell).addEventListener("click", close);
    $(".ce-new-btn", shell).addEventListener("click", createNew);
    $(".ce-textarea", shell).addEventListener("input", () => {
      dirty = true;
      $(".ce-status-text", shell).textContent = "Modified";
      $(".ce-status-text", shell).className = "ce-status-text";
    });
    // Cmd+S to save
    $(".ce-textarea", shell).addEventListener("keydown", (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "s") { e.preventDefault(); save(); }
    });
    return shell;
  }

  async function loadFileList() {
    const shell = renderShell();
    const fileList = $(".ce-file-list", shell);
    try {
      if (mode === "mcp") {
        const data = await api("GET", "/api/config/mcp");
        items = data.configs || [];
      } else if (mode === "skills") {
        const data = await api("GET", "/api/config/skills");
        items = data.skills || [];
      } else if (mode === "claudemd") {
        const data = await api("GET", "/api/config/claudemd");
        items = data.files || [];
      }
    } catch (e) {
      fileList.innerHTML = `<div style="padding:12px;color:var(--danger);font-size:12px">Failed to load: ${e.message}</div>`;
      return;
    }
    fileList.innerHTML = "";
    for (const item of items) {
      const btn = document.createElement("button");
      btn.className = "ce-file-item";
      const label = mode === "mcp" ? item.label
        : mode === "claudemd" ? item.label
        : item.name;
      const scope = mode === "mcp" ? `${item.servers?.length || 0} srv`
        : mode === "claudemd" ? (item.scope === "global" ? "global" : "project")
        : (item.scope === "global" ? "global" : "project");
      const exists = item.exists !== false;
      btn.innerHTML = `<span>${escapeHtml(label)}</span>${!exists ? '<span class="ce-scope" style="color:var(--muted)">new</span>' : `<span class="ce-scope">${scope}</span>`}`;
      if (mode === "skills") {
        const del = document.createElement("button");
        del.className = "ce-delete";
        del.textContent = "×";
        del.title = "Delete skill";
        del.addEventListener("click", async (e) => {
          e.stopPropagation();
          if (!confirm(`Delete "${item.name}"?`)) return;
          try {
            await api("POST", "/api/config/skill/delete", { path: item.path });
            toast("Deleted");
            loadFileList();
          } catch (err) { toast(err.message, { error: true }); }
        });
        btn.append(del);
      }
      btn.addEventListener("click", () => selectItem(item, btn));
      fileList.append(btn);
    }
    if (items.length && !activeItem) {
      const firstBtn = $(".ce-file-item", fileList);
      if (firstBtn) selectItem(items[0], firstBtn);
    }
  }

  async function selectItem(item, btnEl) {
    if (dirty && !confirm("Discard unsaved changes?")) return;
    activeItem = item;
    dirty = false;
    $$(".ce-file-item").forEach((b) => b.classList.remove("active"));
    if (btnEl) btnEl.classList.add("active");

    const shell = $(".config-editor-root");
    if (!shell) return;
    $(".config-editor-path", shell).textContent = item.path || item.name;
    const textarea = $(".ce-textarea", shell);
    const statusText = $(".ce-status-text", shell);
    statusText.textContent = "Loading…";
    statusText.className = "ce-status-text";

    try {
      const endpoint = mode === "mcp" ? "/api/config/mcp/read"
        : mode === "claudemd" ? "/api/config/claudemd/read"
        : "/api/config/skill/read";
      const data = await api("POST", endpoint, { path: item.path });
      textarea.value = data.content || "";
      statusText.textContent = "Ready";
    } catch (e) {
      textarea.value = "";
      statusText.textContent = `Error: ${e.message}`;
      statusText.className = "ce-status-text error";
    }
  }

  async function save() {
    if (!activeItem) return;
    const shell = $(".config-editor-root");
    if (!shell) return;
    const textarea = $(".ce-textarea", shell);
    const statusText = $(".ce-status-text", shell);

    try {
      const endpoint = mode === "mcp" ? "/api/config/mcp/write"
        : mode === "claudemd" ? "/api/config/claudemd/write"
        : "/api/config/skill/write";
      await api("POST", endpoint, { path: activeItem.path, content: textarea.value });
      dirty = false;
      statusText.textContent = "Saved ✓";
      statusText.className = "ce-status-text saved";
      setTimeout(() => { if (statusText.textContent === "Saved ✓") statusText.textContent = "Ready"; }, 2000);
    } catch (e) {
      statusText.textContent = `Save failed: ${e.message}`;
      statusText.className = "ce-status-text error";
    }
  }

  async function createNew() {
    const prompts = { mcp: "Path for new MCP config:", skills: "Skill name (e.g. my-skill):", claudemd: "Path for new CLAUDE.md:" };
    const name = prompt(prompts[mode] || "Name:");
    if (!name) return;
    try {
      if (mode === "mcp") {
        const p = name.startsWith("/") || name.startsWith("~") ? name : `~/.claude/${name}`;
        await api("POST", "/api/config/mcp/write", { path: p, content: JSON.stringify({ mcpServers: {} }, null, 2) });
      } else if (mode === "claudemd") {
        const p = name.startsWith("/") || name.startsWith("~") ? name : `${name}/CLAUDE.md`;
        await api("POST", "/api/config/claudemd/write", { path: p, content: "# CLAUDE.md\n\n" });
      } else {
        const scope = prompt("Scope — type 'global' or a project path:", "global");
        if (!scope) return;
        await api("POST", "/api/config/skill/create", { scope, name });
      }
      toast("Created");
      loadFileList();
    } catch (e) {
      toast(`Failed: ${e.message}`, { error: true });
    }
  }

  return { open, close, get mode() { return mode; } };
})();

// Wire sidebar config nav buttons
$$(".config-nav").forEach((btn) =>
  btn.addEventListener("click", () => {
    const m = btn.dataset.config;
    if (!m) return;
    if (configEditor.mode === m) configEditor.close();
    else configEditor.open(m);
  })
);

/* Events — filter dropdown */
(() => {
  const filterBtn = $("#filterBtn");
  const filterMenu = $("#filterMenu");
  if (!filterBtn || !filterMenu) return;
  filterBtn.addEventListener("click", (e) => {
    e.stopPropagation();
    filterMenu.classList.toggle("open");
  });
  document.addEventListener("click", () => filterMenu.classList.remove("open"));
  filterMenu.addEventListener("click", (e) => e.stopPropagation());
  $$("[data-filter]", filterMenu).forEach((btn) =>
    btn.addEventListener("click", () => {
      state.filter = btn.dataset.filter;
      state.groupFilter = null;
      state.lastListSig = "";
      filterMenu.classList.remove("open");
      renderNav();
      renderGroupNav();
      renderList();
    })
  );
  const showEndedCb = $("#showEnded");
  if (showEndedCb) showEndedCb.addEventListener("change", (e) => {
    state.showEnded = e.target.checked;
    state.lastListSig = "";
    renderNav();
    renderList();
  });
})();

$("#search").addEventListener("input", (e) => {
  state.query = e.target.value;
  state.lastListSig = "";
  renderList();
});

$("#newGroupBtn").addEventListener("click", promptNewGroup);

async function showNewInstanceModal() {
  const tpl = $("#newInstanceModalTpl");
  if (!tpl) return;
  const bd = tpl.content.firstElementChild.cloneNode(true);
  document.body.append(bd);
  const close = () => bd.remove();
  const cwdInput = $(".cwd-input", bd);
  const cmdInput = $(".cmd-input", bd);
  const nameInput = $(".name-input", bd);
  const datalist = $("#recentCwdsList", bd);
  const recentBox = $(".recent-cwds", bd);

  try {
    const { cwds } = await api("GET", "/api/recent-cwds");
    for (const c of cwds) {
      const opt = document.createElement("option");
      opt.value = c;
      datalist.append(opt);
    }
    for (const c of cwds.slice(0, 6)) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "recent-cwd-chip";
      btn.textContent = c.replace(/^\/Users\/[^/]+/, "~");
      btn.title = c;
      btn.addEventListener("click", () => {
        cwdInput.value = c;
        cwdInput.focus();
      });
      recentBox.append(btn);
    }
  } catch {}

  // MCP source: editable path + quick-pick pills + dynamic checkbox list.
  const mcpPicker = $(".mcp-picker", bd);
  const mcpSourceInput = $(".mcp-source-input", bd);
  const mcpSourcePills = $(".mcp-source-pills", bd);
  let useMcps = null;

  const renderMcpCheckboxes = (names) => {
    mcpPicker.innerHTML = "";
    if (!names || !names.length) {
      mcpPicker.innerHTML = `<div class="mcp-picker-empty">No MCPs in this file</div>`;
      useMcps = () => [];
      return;
    }
    const saved = JSON.parse(localStorage.getItem("ciu_last_mcps") || "null");
    for (const name of names) {
      const row = document.createElement("label");
      row.className = "mcp-row";
      const checked = saved ? saved.includes(name) : true;
      row.innerHTML = `<input type="checkbox" value="${escapeHtml(name)}" ${checked ? "checked" : ""} /><span class="mcp-name">${escapeHtml(name)}</span>`;
      mcpPicker.append(row);
    }
    const quick = document.createElement("div");
    quick.className = "mcp-quick";
    quick.innerHTML = `<button type="button" data-act="all">all</button><button type="button" data-act="none">none</button>`;
    mcpPicker.append(quick);
    quick.addEventListener("click", (e) => {
      const act = e.target.dataset.act;
      if (!act) return;
      for (const cb of $$('input[type=checkbox]', mcpPicker)) cb.checked = act === "all";
    });
    useMcps = () => $$("input[type=checkbox]:checked", mcpPicker).map((c) => c.value);
  };

  const loadFromPath = async (path) => {
    const p = (path || "").trim();
    if (!p) {
      mcpPicker.innerHTML = `<div class="mcp-picker-empty">Set a source path above</div>`;
      useMcps = null;
      return;
    }
    mcpPicker.innerHTML = `<div class="mcp-picker-empty">Loading…</div>`;
    try {
      const res = await api("POST", "/api/mcp-list", { path: p });
      if (!res.exists) {
        mcpPicker.innerHTML = `<div class="mcp-picker-empty">File not found: <code>${escapeHtml(res.path)}</code></div>`;
        useMcps = () => [];
        return;
      }
      renderMcpCheckboxes(res.mcps);
    } catch (e) {
      mcpPicker.innerHTML = `<div class="mcp-picker-empty">Failed: ${escapeHtml(e.message)}</div>`;
      useMcps = null;
    }
  };

  // Fetch known sources for quick-pick pills (failure is non-fatal)
  let knownSources = [];
  try {
    const { sources } = await api("GET", "/api/mcp-sources");
    knownSources = sources || [];
  } catch {}

  mcpSourcePills.innerHTML = "";
  const pillsToShow = knownSources.length
    ? knownSources
    : [
        { path: "~/.claude.json", label: "~/.claude.json", exists: true, count: null },
        { path: "~/.claude/mcp.json", label: "~/.claude/mcp.json", exists: true, count: null },
      ];
  for (const s of pillsToShow) {
    const pill = document.createElement("button");
    pill.type = "button";
    pill.className = "mcp-source-pill";
    if (s.exists === false) pill.classList.add("missing");
    pill.title = s.path;
    pill.innerHTML = `<span class="mss-label">${escapeHtml(s.label || s.path)}</span>` +
      (s.count != null ? `<span class="mss-count">${s.count}</span>` : "");
    pill.addEventListener("click", () => {
      mcpSourceInput.value = s.path;
      localStorage.setItem("ciu_last_mcp_source", s.path);
      loadFromPath(s.path);
    });
    mcpSourcePills.append(pill);
  }

  // Pre-fill the input and load
  const savedPath = localStorage.getItem("ciu_last_mcp_source") || "";
  const firstExisting = knownSources.find((s) => s.exists);
  const initialPath = savedPath || (firstExisting ? firstExisting.path : "~/.claude.json");
  mcpSourceInput.value = initialPath;
  loadFromPath(initialPath);

  let loadTimer = null;
  mcpSourceInput.addEventListener("input", () => {
    clearTimeout(loadTimer);
    loadTimer = setTimeout(() => {
      const v = mcpSourceInput.value.trim();
      if (v) localStorage.setItem("ciu_last_mcp_source", v);
      loadFromPath(v);
    }, 350);
  });

  const submit = async () => {
    const cwd = cwdInput.value.trim();
    const command = cmdInput.value.trim() || "claude";
    if (!cwd) { cwdInput.focus(); return; }
    try {
      const resolved = cwd.startsWith("~") ? cwd.replace(/^~/, getHome()) : cwd;
      const payload = { cwd: resolved, command };
      const nm = (nameInput.value || "").trim();
      if (nm) payload.name = nm;
      if (typeof useMcps === "function") {
        const selected = useMcps();
        payload.mcps = selected;
        const src = (mcpSourceInput.value || "").trim();
        if (src) payload.mcp_source = src;
        localStorage.setItem("ciu_last_mcps", JSON.stringify(selected));
      }
      const res = await api("POST", "/api/instances/new", payload);
      close();
      toast("Starting…");
      // Poll up to 5s for the new session to appear, then select it
      const deadline = Date.now() + 5000;
      const tick = async () => {
        await refresh();
        const match = state.instances.find((i) => i.our_sid === res.our_sid);
        if (match) {
          selectInstance(match.session_id);
          return;
        }
        if (Date.now() < deadline) setTimeout(tick, 300);
      };
      tick();
    } catch (e) {
      toast(`Failed: ${e.message}`, { error: true });
    }
  };

  $(".cancel", bd).addEventListener("click", close);
  $(".ok", bd).addEventListener("click", submit);
  bd.addEventListener("click", (e) => { if (e.target === bd) close(); });
  bd.addEventListener("keydown", (e) => {
    if (e.key === "Escape") close();
    if (e.key === "Enter" && (e.target === cwdInput || e.target === cmdInput)) submit();
  });
  setTimeout(() => nameInput.focus(), 20);
}

function getHome() {
  // Best-effort: infer from any known cwd that starts with /Users/
  for (const i of state.instances) {
    const m = (i.cwd || "").match(/^(\/Users\/[^/]+)/);
    if (m) return m[1];
  }
  return "";
}

const newInstanceBtn = $("#newInstanceBtn");
if (newInstanceBtn) newInstanceBtn.addEventListener("click", showNewInstanceModal);

/* ------------------------------------------------------------------ */
/*  File upload — clipboard paste + drag-and-drop                     */
/* ------------------------------------------------------------------ */

async function uploadAndInsertFiles(sid, files) {
  if (!sid || !files || !files.length) return;
  if (xtermManager.sid !== sid) {
    toast("Select the instance first", { error: true });
    return;
  }

  const t = toast.bind(null);
  t(files.length === 1
    ? `↑ uploading ${files[0].name || "image"}…`
    : `↑ uploading ${files.length} files…`);

  const paths = [];
  for (const f of files) {
    try {
      const fd = new FormData();
      fd.append("file", f, f.name || "image.png");
      const headers = {};
      if (AUTH_TOKEN) headers["Authorization"] = `Bearer ${AUTH_TOKEN}`;
      const res = await fetch(`/api/instances/${sid}/upload`, {
        method: "POST", headers, body: fd,
      });
      if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
      const data = await res.json();
      paths.push(data.path);
    } catch (err) {
      toast(`Upload failed: ${err.message}`, { error: true });
    }
  }

  if (!paths.length) return;

  // Inject the plain path(s) into the terminal; user presses Enter themselves
  // once they've typed any surrounding prompt text.
  const insertion = paths.map((p) => p.includes(" ") ? `"${p}"` : p).join(" ") + " ";
  if (!xtermManager.writeText(insertion)) {
    toast("Terminal not connected", { error: true });
    return;
  }
  xtermManager.focus();
  toast(`Attached ${paths.length} file${paths.length > 1 ? "s" : ""}`);
}

// Clipboard paste — anywhere on the page, if there's an image file, grab it.
// Capture phase so xterm's own paste handler doesn't also run.
document.addEventListener("paste", async (e) => {
  const sid = state.selectedSid;
  if (!sid) return;
  const items = e.clipboardData ? Array.from(e.clipboardData.items) : [];
  const files = items
    .filter((it) => it.kind === "file")
    .map((it) => it.getAsFile())
    .filter(Boolean);
  if (!files.length) return;
  e.preventDefault();
  e.stopPropagation();
  await uploadAndInsertFiles(sid, files);
}, true);

// Drag-and-drop: accept files dropped anywhere on the preview pane.
(function wirePreviewDrop() {
  const previewCol = document.getElementById("previewCol");
  if (!previewCol) return;
  let dragDepth = 0;
  const markDrop = (on) => {
    const host = document.getElementById("xtermHost");
    if (host) host.classList.toggle("drop-target", on);
    previewCol.classList.toggle("drop-target", on);
  };
  const onDragEnter = (e) => {
    if (!e.dataTransfer || !Array.from(e.dataTransfer.types || []).includes("Files")) return;
    e.preventDefault();
    dragDepth++;
    markDrop(true);
  };
  const onDragLeave = () => {
    dragDepth = Math.max(0, dragDepth - 1);
    if (dragDepth === 0) markDrop(false);
  };
  const onDragOver = (e) => {
    if (!e.dataTransfer || !Array.from(e.dataTransfer.types || []).includes("Files")) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
  };
  const onDrop = async (e) => {
    if (!e.dataTransfer?.files?.length) return;
    e.preventDefault();
    dragDepth = 0;
    markDrop(false);
    const sid = state.selectedSid;
    if (!sid) { toast("Select an instance first", { error: true }); return; }
    await uploadAndInsertFiles(sid, Array.from(e.dataTransfer.files));
  };
  previewCol.addEventListener("dragenter", onDragEnter);
  previewCol.addEventListener("dragleave", onDragLeave);
  previewCol.addEventListener("dragover", onDragOver);
  previewCol.addEventListener("drop", onDrop);
  // Also stop the browser from opening files dropped outside the preview.
  window.addEventListener("dragover", (e) => {
    if (e.dataTransfer?.types?.includes?.("Files")) e.preventDefault();
  });
  window.addEventListener("drop", (e) => {
    if (e.dataTransfer?.types?.includes?.("Files") && !previewCol.contains(e.target)) {
      e.preventDefault();
    }
  });
})();

// Sidebar collapse/expand — persists in localStorage
(function () {
  const sidebar = document.getElementById("sidebar");
  const toggle = document.getElementById("sidebarToggle");
  if (!sidebar || !toggle) return;
  const apply = (collapsed) => {
    document.body.classList.add("sidebar-transitioning");
    document.body.classList.toggle("sidebar-collapsed", collapsed);
    toggle.textContent = collapsed ? "›" : "‹";
    toggle.title = collapsed ? "Expand sidebar" : "Collapse sidebar";
    setTimeout(() => {
      document.body.classList.remove("sidebar-transitioning");
      xtermManager.fit();
    }, 220);
  };
  apply(localStorage.getItem("ciu_sidebar_collapsed") === "1");
  toggle.addEventListener("click", (e) => {
    e.stopPropagation();
    const next = !document.body.classList.contains("sidebar-collapsed");
    localStorage.setItem("ciu_sidebar_collapsed", next ? "1" : "0");
    apply(next);
  });
  sidebar.addEventListener("click", (e) => {
    if (!document.body.classList.contains("sidebar-collapsed")) return;
    if (e.target.closest(".config-nav") || e.target.closest(".group-nav-item")) return;
    localStorage.setItem("ciu_sidebar_collapsed", "0");
    apply(false);
  });
})();

$("#openFullBtn").addEventListener("click", () => {
  api("POST", "/api/open-dashboard").catch((e) => toast(e.message, { error: true }));
});

document.addEventListener("keydown", (e) => {
  if (e.key === "/" && document.activeElement.tagName !== "INPUT" && document.activeElement.tagName !== "TEXTAREA") {
    const xtHost = document.querySelector(".xterm-helper-textarea");
    if (xtHost && document.activeElement === xtHost) return; // xterm has focus → let it through
    e.preventDefault();
    $("#search").focus();
    return;
  }
  if (e.key === "Escape" && document.activeElement === $("#search")) {
    $("#search").value = "";
    state.query = "";
    state.lastListSig = "";
    renderList();
    $("#search").blur();
    return;
  }
});

/* ------------------------------------------------------------------ */
/*  Settings panel (Hammerspoon menu bar)                              */
/* ------------------------------------------------------------------ */
function showSettingsPanel() {
  const tpl = $("#settingsTpl");
  if (!tpl) return;
  const bd = tpl.content.firstElementChild.cloneNode(true);
  document.body.append(bd);

  const status = bd.querySelector("#hsStatus");
  const soundCb = bd.querySelector("#hsSound");
  const ttlInput = bd.querySelector("#hsBannerTtl");
  const pollInput = bd.querySelector("#hsPollInterval");

  api("GET", "/api/settings").then((s) => {
    soundCb.checked = s.sound !== false;
    ttlInput.value = s.banner_ttl || 30;
    pollInput.value = s.poll_interval || 2;
    const parts = [];
    parts.push(s.hs_installed ? "Module installed" : "Module not installed");
    parts.push(s.hs_running ? "Hammerspoon running" : "Hammerspoon not running");
    status.textContent = parts.join(" · ");
    status.className = "settings-status " + (s.hs_installed && s.hs_running ? "ok" : "warn");
  }).catch(() => {
    status.textContent = "Could not load settings";
    status.className = "settings-status warn";
  });

  const close = () => bd.remove();
  bd.querySelector(".cancel").addEventListener("click", close);
  bd.addEventListener("click", (e) => { if (e.target === bd) close(); });
  bd.addEventListener("keydown", (e) => { if (e.key === "Escape") close(); });

  bd.querySelector(".save-settings").addEventListener("click", async () => {
    try {
      await api("PUT", "/api/settings", {
        sound: soundCb.checked,
        banner_ttl: parseInt(ttlInput.value) || 30,
        poll_interval: parseInt(pollInput.value) || 2,
      });
      toast("Settings saved");
      close();
    } catch (e) {
      toast(`Save failed: ${e.message}`, { error: true });
    }
  });

  bd.querySelector("#hsInstall").addEventListener("click", async () => {
    try {
      await api("POST", "/api/settings/install-hammerspoon");
      toast("Module installed — reload Hammerspoon to apply");
      status.textContent = "Module installed · reload Hammerspoon to apply";
      status.className = "settings-status ok";
    } catch (e) {
      toast(`Install failed: ${e.message}`, { error: true });
    }
  });

  bd.querySelector("#hsReload").addEventListener("click", async () => {
    try {
      await api("POST", "/api/settings/reload-hammerspoon");
      toast("Reload signal sent");
    } catch (e) {
      toast(`Reload failed: ${e.message}`, { error: true });
    }
  });
}

const settingsBtn = $("#settingsHammerspoon");
if (settingsBtn) settingsBtn.addEventListener("click", showSettingsPanel);

if (new URLSearchParams(location.search).get("compact") === "1") {
  document.documentElement.classList.add("compact");
  document.body.classList.add("compact");
}

refresh();
setInterval(refresh, 2000);
