const state = {
  filter: "all",
  groupFilter: null,
  query: "",
  showEnded: false,
  instances: [],
  selectedSid: null,
  transcriptData: null,
  renderedUuids: new Set(),
  renderedSid: null,
  pinScroll: true,
  lastListSig: "",
  pendingAcks: {},
  activeTerm: null, // {terminal, fitAddon, ws, sid, host, resizeHandler}
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

async function api(method, url, body) {
  const res = await fetch(url, {
    method,
    headers: body ? { "Content-Type": "application/json" } : {},
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
      // In compact tray mode: open the full dashboard focused on this sid
      api("POST", "/api/open-dashboard").catch(() => {});
      window.location.href = "ciu://close";
      return;
    }
    selectInstance(inst.session_id);
  });
  node.addEventListener("dblclick", (e) => {
    e.preventDefault();
    // Double-click focuses the terminal (if already selected + tmux-owned)
    if (state.selectedSid === inst.session_id && state.activeTerm) {
      try { state.activeTerm.terminal.focus(); } catch {}
    } else {
      selectInstance(inst.session_id);
    }
  });
  wireDragAndDrop(node, inst.session_id);
  return node;
}

function updateListCard(node, inst) {
  const intensity = freshIntensity(inst);
  const isFresh = intensity > 0;
  node.className = `list-card ${inst.status}` + (isFresh ? " fresh" : "") + (inst.session_id === state.selectedSid ? " selected" : "");
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
  if (state.selectedSid === sid) return;
  unmountTerminal();
  state.selectedSid = sid;
  state.transcriptData = null;
  state.renderedUuids = new Set();
  state.renderedSid = null;
  state.pinScroll = true;
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
    .join("#") + `§${state.selectedSid || ""}§${state.query}§${state.filter}§${state.groupFilter || ""}§${state.showEnded}`;
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
    if (needsHeaders) {
      const hdr = document.createElement("div");
      hdr.className = "list-group-label";
      hdr.dataset.group = key;
      hdr.innerHTML = `<span class="lg-name">${escapeHtml(key)}</span><span class="g-count">${buckets.get(key).length}</span>`;
      if (key !== "Ungrouped") {
        hdr.style.cursor = "text";
        hdr.title = "Click to rename";
        hdr.addEventListener("dblclick", (e) => {
          e.preventDefault();
          startInlineRename(hdr, key);
        });
        wireGroupHeaderDrop(hdr, key);
      }
      list.append(hdr);
    }
    for (const inst of buckets.get(key)) list.append(renderListCard(inst));
  }
}

/* Terminal (xterm.js) */
function mountTerminal(host, sessionId) {
  unmountTerminal();
  if (!window.Terminal) {
    host.textContent = "xterm.js failed to load (check network)";
    return;
  }
  host.innerHTML = "";
  const term = new window.Terminal({
    cursorBlink: true,
    fontFamily: "ui-monospace, Menlo, Consolas, monospace",
    fontSize: 13,
    lineHeight: 1.15,
    scrollback: 10000,
    allowProposedApi: true,
    theme: {
      background: "#0b0d10",
      foreground: "#d6d6d6",
      cursor: "#ffbf69",
      selectionBackground: "#ffbf6933",
    },
  });
  let fitAddon = null;
  if (window.FitAddon && window.FitAddon.FitAddon) {
    fitAddon = new window.FitAddon.FitAddon();
    term.loadAddon(fitAddon);
  }
  term.open(host);
  if (fitAddon) {
    try { fitAddon.fit(); } catch {}
  }

  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(`${proto}//${location.host}/ws/instances/${sessionId}/terminal`);
  ws.binaryType = "arraybuffer";

  const sendResize = () => {
    if (ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
  };

  ws.addEventListener("open", () => {
    if (fitAddon) {
      try { fitAddon.fit(); } catch {}
    }
    sendResize();
  });
  ws.addEventListener("message", (e) => {
    if (e.data instanceof ArrayBuffer) {
      term.write(new Uint8Array(e.data));
    } else {
      term.write(e.data);
    }
  });
  ws.addEventListener("close", () => {
    term.write("\r\n\x1b[90m[disconnected]\x1b[0m\r\n");
  });
  ws.addEventListener("error", () => {
    term.write("\r\n\x1b[31m[websocket error]\x1b[0m\r\n");
  });

  term.onData((data) => {
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "input", data }));
    }
  });

  term.onResize(() => sendResize());

  const resizeHandler = () => {
    if (fitAddon) {
      try { fitAddon.fit(); } catch {}
    }
  };
  window.addEventListener("resize", resizeHandler);

  state.activeTerm = { terminal: term, fitAddon, ws, sid: sessionId, host, resizeHandler };
  setTimeout(() => { try { term.focus(); } catch {} }, 30);
}

function unmountTerminal() {
  const at = state.activeTerm;
  if (!at) return;
  state.activeTerm = null;
  try { window.removeEventListener("resize", at.resizeHandler); } catch {}
  try { at.ws.close(); } catch {}
  try { at.terminal.dispose(); } catch {}
  if (at.host) at.host.innerHTML = "";
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
  const { session, entries } = state.transcriptData;
  const shell = ensurePreviewShell();

  const sidChanged = state.renderedSid !== session.session_id;
  const hasTmux = !!session.tmux_session;

  if (sidChanged) {
    state.renderedUuids = new Set();
    state.renderedSid = session.session_id;
    const log = $("#termLog", shell);
    if (log) log.innerHTML = "";
    state.pinScroll = true;
  }

  shell.className = `preview-root ${session.status}` + (hasTmux ? " has-terminal" : "");
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
    banner.textContent = session.notification_message;
    banner.classList.add("show");
  } else {
    banner.classList.remove("show");
  }

  renderSummary(shell, session.summary);
  renderSubagents(shell, session.subagents || []);

  const focusBtn = $(".ph-focus", shell);

  // Branch: tmux-owned → xterm.js. Otherwise → read-only transcript view.
  if (hasTmux) {
    const host = $("#xtermHost", shell);
    const needsMount = !state.activeTerm || state.activeTerm.sid !== session.session_id;
    if (needsMount && host) mountTerminal(host, session.session_id);
    focusBtn.disabled = false;
    focusBtn.title = "Focus terminal";
    return;
  }

  // External (non-tmux) session path: transcript preview
  unmountTerminal();
  focusBtn.disabled = true;
  focusBtn.title = "External session — no interactive terminal available";

  const tpPath = $(".tp-path", shell);
  if (tpPath) tpPath.textContent = truncCwd(session.cwd);

  const footerStatus = $(".term-footer-status", shell);
  if (footerStatus) {
    if (session.status === "running") {
      footerStatus.innerHTML = `<span style="color:var(--accent-bright)">▸ running</span>${session.last_tool ? ` · <span style="color:var(--muted-strong)">${escapeHtml(session.last_tool)}</span>` : ""}`;
    } else if (session.status === "needs_input") {
      footerStatus.innerHTML = `<span style="color:var(--accent-bright)">! waiting for you</span>`;
    } else if (session.status === "idle") {
      footerStatus.innerHTML = `<span style="color:var(--muted-strong)">● idle</span>${session.hook_timestamp ? ` · ${relTime(session.hook_timestamp)} ago` : ""}`;
    } else {
      footerStatus.innerHTML = `<span style="color:var(--muted)">× ended</span>`;
    }
  }

  renderTermLog(shell, entries);
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

function renderTermLog(shell, entries) {
  const log = $("#termLog", shell);
  const wasAtBottom = isAtBottom(log);

  if (!entries.length && !log.childElementCount) {
    const empty = document.createElement("div");
    empty.className = "term-empty";
    empty.textContent = "# no transcript yet";
    log.append(empty);
    return;
  }
  if (entries.length && log.querySelector(".term-empty")) {
    log.innerHTML = "";
  }

  let appended = 0;
  for (const entry of entries) {
    const key = entry.uuid || `${entry.timestamp}-${entry.type}`;
    if (state.renderedUuids.has(key)) continue;
    state.renderedUuids.add(key);
    const lines = buildTermLines(entry);
    for (const line of lines) log.append(line);
    appended += lines.length;
  }

  if (appended && (state.pinScroll || wasAtBottom)) {
    log.classList.add("no-smooth");
    log.scrollTop = log.scrollHeight;
    requestAnimationFrame(() => log.classList.remove("no-smooth"));
  }
  updateScrollPin(shell);
}

function buildTermLines(entry) {
  const lines = [];
  const ts = fmtTime(entry.timestamp);

  for (const p of entry.parts) {
    if (p.kind === "text" && (p.text || "").trim()) {
      lines.push(mkLine(entry.type, ts, p.text));
    } else if (p.kind === "thinking" && (p.text || "").trim()) {
      lines.push(mkLine("thinking", ts, p.text, "💭"));
    } else if (p.kind === "tool_use") {
      lines.push(mkToolLine(ts, p));
    } else if (p.kind === "tool_result") {
      lines.push(mkResultLine(ts, p));
    }
  }
  return lines;
}

function mkLine(type, ts, text, glyphOverride) {
  const div = document.createElement("div");
  div.className = `term-line ${type}`;
  const glyph = glyphOverride || (type === "user" ? "›" : type === "thinking" ? "≈" : "‹");
  div.innerHTML = `
    <span class="tl-time">${ts}</span>
    <span class="tl-glyph">${glyph}</span>
    <span class="tl-body"></span>`;
  $(".tl-body", div).textContent = text;
  return div;
}

function mkToolLine(ts, p) {
  const div = document.createElement("div");
  div.className = "term-line tool";
  const summary = summarizeTool(p.tool, p.input);
  div.innerHTML = `
    <span class="tl-time">${ts}</span>
    <span class="tl-glyph">▶</span>
    <span class="tl-body"></span>`;
  const body = $(".tl-body", div);
  const tool = document.createElement("span");
  tool.className = "tl-tool";
  tool.textContent = p.tool || "tool";
  body.append(tool);
  if (summary) {
    body.append(" ");
    const args = document.createElement("span");
    args.className = "tl-args";
    args.textContent = summary;
    body.append(args);
  }
  return div;
}

function mkResultLine(ts, p) {
  const div = document.createElement("div");
  div.className = `term-line result${p.is_error ? " error" : ""}`;
  const text = (p.text || "").trim();
  const short = text.length > 400;
  div.innerHTML = `
    <span class="tl-time">${ts}</span>
    <span class="tl-glyph">◂</span>
    <span class="tl-body"></span>`;
  $(".tl-body", div).textContent = short ? text.slice(0, 400) : text;
  if (short) {
    const btn = document.createElement("button");
    btn.className = "expand-btn";
    btn.textContent = `[+] show ${text.length - 400} more chars`;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      div.classList.toggle("expanded");
      if (div.classList.contains("expanded")) {
        $(".tl-body", div).textContent = text;
        btn.textContent = "[−] collapse";
      } else {
        $(".tl-body", div).textContent = text.slice(0, 400);
        btn.textContent = `[+] show ${text.length - 400} more chars`;
      }
    });
    $(".tl-body", div).appendChild(document.createElement("br"));
    $(".tl-body", div).appendChild(btn);
  }
  return div;
}

function summarizeTool(tool, input) {
  if (!input) return "";
  if (tool === "Bash" && input.command) return input.command;
  if (tool === "Read" && input.file_path) return input.file_path;
  if (tool === "Edit" && input.file_path) return input.file_path;
  if (tool === "Write" && input.file_path) return input.file_path;
  if (tool === "Grep" && input.pattern) return `"${input.pattern}"${input.path ? " in " + input.path : ""}`;
  if (tool === "Glob" && input.pattern) return input.pattern;
  if (tool === "WebFetch" && input.url) return input.url;
  if (tool === "WebSearch" && input.query) return `"${input.query}"`;
  if (tool === "TodoWrite") return `${(input.todos || []).length} todos`;
  if (tool === "Task" && input.description) return input.description;
  try {
    const s = JSON.stringify(input);
    return s.length > 200 ? s.slice(0, 200) + "…" : s;
  } catch {
    return "";
  }
}

function isAtBottom(el) {
  return Math.abs(el.scrollHeight - el.scrollTop - el.clientHeight) < 30;
}

function updateScrollPin(shell) {
  const log = $("#termLog", shell);
  const pin = $("#scrollPin", shell);
  if (isAtBottom(log)) {
    state.pinScroll = true;
    pin.classList.remove("unpinned");
    pin.textContent = "↓ live";
  } else {
    state.pinScroll = false;
    pin.classList.add("unpinned");
    pin.textContent = "↓ jump to live";
  }
}

function wirePreviewActions(shell) {
  const sess = () => state.transcriptData?.session;

  const focusBtnWire = $(".ph-focus", shell);
  focusBtnWire.addEventListener("click", () => {
    if (state.activeTerm) {
      try { state.activeTerm.terminal.focus(); } catch {}
    }
  });

  $(".ph-rename", shell).addEventListener("click", () => {
    const s = sess(); if (!s) return;
    showModal({
      title: "Rename instance",
      value: s.custom_name || s.name,
      placeholder: "e.g. Grafana work",
      onSubmit: async (v) => {
        await api("PUT", `/api/instances/${s.session_id}/name`, { name: v });
        refresh();
      },
    });
  });

  $(".ph-group", shell).addEventListener("click", () => {
    const s = sess(); if (!s) return;
    const existing = [...new Set(state.instances.map((i) => i.group).filter(Boolean))];
    showModal({
      title: "Set group",
      value: s.group || "",
      placeholder: "Group name (blank = ungrouped)",
      suggestions: existing,
      onSubmit: async (v) => {
        await api("PUT", `/api/instances/${s.session_id}/group`, { group: v || null });
        refresh();
      },
    });
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
      if (act === "sigint") {
        await api("POST", `/api/instances/${s.session_id}/signal`, { signal: "INT" });
        toast("SIGINT sent");
      } else if (act === "sigterm") {
        if (!confirm(`SIGTERM this session?`)) return;
        await api("POST", `/api/instances/${s.session_id}/signal`, { signal: "TERM" });
        toast("SIGTERM sent");
      } else if (act === "kill") {
        if (!s.tmux_session) {
          toast("Not a tmux-owned session", { error: true });
          return;
        }
        if (!confirm(`Kill tmux session ${s.tmux_session}?`)) return;
        await api("POST", `/api/instances/${s.session_id}/kill`);
        unmountTerminal();
        toast("Session killed");
        refresh();
      } else if (act === "forget") {
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

  const log = $("#termLog", shell);
  log.addEventListener("scroll", () => updateScrollPin(shell));
  $("#scrollPin", shell).addEventListener("click", () => {
    state.pinScroll = true;
    log.scrollTop = log.scrollHeight;
    updateScrollPin(shell);
  });
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

/* Sidebar */
function renderNav() {
  const live = state.instances.filter((i) => i.alive);
  const counts = {
    all: state.showEnded ? state.instances.length : live.length,
    running: live.filter((i) => i.status === "running").length,
    idle: live.filter((i) => i.status === "idle").length,
    needs_input: live.filter((i) => i.status === "needs_input").length,
    ended: state.instances.filter((i) => !i.alive).length,
  };
  $$(".nav[data-filter]").forEach((btn) => {
    const f = btn.dataset.filter;
    btn.classList.toggle("active", state.filter === f && !state.groupFilter);
    $(".count", btn).textContent = counts[f] ?? "";
  });
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
    row.innerHTML = `
      <span class="dot" style="background:var(--accent)"></span>
      <span class="g-name">${escapeHtml(g)}</span>
      <span class="count">${state.instances.filter((i) => i.group === g).length}</span>
      <button class="del" title="Delete group">×</button>`;
    row.addEventListener("click", (e) => {
      if (e.target.closest(".del")) return;
      state.groupFilter = state.groupFilter === g ? null : g;
      renderList();
      renderGroupNav();
      renderNav();
    });
    $(".del", row).addEventListener("click", async (e) => {
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
async function refresh() {
  if (_refreshing) return;
  _refreshing = true;
  try {
    const { instances, served_at } = await api("GET", "/api/instances");
    state.instances = instances;
    $("#updated").textContent = `updated ${new Date(served_at * 1000).toLocaleTimeString()}`;
    renderNav();
    renderGroupNav();
    renderList();
    if (state.selectedSid) await loadTranscript();
  } catch (e) {
    console.error(e);
  } finally {
    _refreshing = false;
  }
}

/* Events */
$$(".nav[data-filter]").forEach((btn) =>
  btn.addEventListener("click", () => {
    state.filter = btn.dataset.filter;
    state.groupFilter = null;
    state.lastListSig = "";
    renderNav();
    renderGroupNav();
    renderList();
  })
);

$("#search").addEventListener("input", (e) => {
  state.query = e.target.value;
  state.lastListSig = "";
  renderList();
});

$("#showEnded").addEventListener("change", (e) => {
  state.showEnded = e.target.checked;
  state.lastListSig = "";
  renderNav();
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

  const submit = async () => {
    const cwd = cwdInput.value.trim();
    const command = cmdInput.value.trim() || "claude";
    if (!cwd) { cwdInput.focus(); return; }
    try {
      const resolved = cwd.startsWith("~") ? cwd.replace(/^~/, getHome()) : cwd;
      const res = await api("POST", "/api/instances/new", { cwd: resolved, command });
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
  setTimeout(() => cwdInput.focus(), 20);
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

$("#openFullBtn").addEventListener("click", () => {
  api("POST", "/api/open-dashboard").catch((e) => toast(e.message, { error: true }));
});

document.addEventListener("keydown", (e) => {
  if (e.key === "/" && document.activeElement.tagName !== "INPUT") {
    e.preventDefault();
    $("#search").focus();
  }
  if (e.key === "Escape" && document.activeElement === $("#search")) {
    $("#search").value = "";
    state.query = "";
    state.lastListSig = "";
    renderList();
    $("#search").blur();
  }
});

if (new URLSearchParams(location.search).get("compact") === "1") {
  document.documentElement.classList.add("compact");
  document.body.classList.add("compact");
}

refresh();
setInterval(refresh, 2000);
