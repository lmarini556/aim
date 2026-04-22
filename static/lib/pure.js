export const FRESH_WINDOW_SECONDS = 300;

export const STATUS_GLYPH = {
  running: "▸",
  idle: "●",
  needs_input: "!",
  ended: "×",
};

export const STATUS_TEXT = {
  running: "RUNNING",
  idle: "IDLE",
  needs_input: "NEEDS INPUT",
  ended: "ENDED",
};

export const GROUP_COLORS = [
  "#ff6b6b", "#ffa94d", "#ffd43b", "#69db7c", "#38d9a9",
  "#4dabf7", "#748ffc", "#b197fc", "#e599f7", "#f06595",
  "#ff922b", "#51cf66", "#3bc9db", "#5c7cfa", "#cc5de8",
];

export function freshIntensity(inst, pendingAcks = {}, now = Date.now()) {
  if (!inst.alive || inst.status !== "idle") return 0;
  if (inst.last_event !== "Stop") return 0;
  const ts = inst.hook_timestamp;
  if (!ts) return 0;
  const serverAck = inst.ack_timestamp || 0;
  const localAck = pendingAcks[inst.session_id] || 0;
  const ack = Math.max(serverAck, localAck);
  if (ts <= ack) return 0;
  const age = now / 1000 - ts;
  if (age < 0) return 1;
  if (age >= FRESH_WINDOW_SECONDS) return 0;
  return 1 - age / FRESH_WINDOW_SECONDS;
}

export function formatAnsiForLog(bytes) {
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

export function summarizeAnsi(bytes) {
  let s;
  try { s = new TextDecoder("utf-8", { fatal: false }).decode(bytes); }
  catch { return ""; }
  const out = [];
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
  const blocks = s.match(/[▀▄█▁▂▃▅▆▇─━═]/g) || [];
  if (blocks.length) out.push(`GLYPHS:${[...new Set(blocks)].join("")}×${blocks.length}`);
  return out.join(" ");
}

export function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

export function relTime(ts, now = Date.now()) {
  if (!ts) return "";
  const d = typeof ts === "string" ? Date.parse(ts) / 1000 : ts;
  const diff = now / 1000 - d;
  if (diff < 5) return "now";
  if (diff < 60) return `${Math.floor(diff)}s`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

export function fmtTime(ts) {
  if (!ts) return "";
  const d = typeof ts === "string" ? new Date(ts) : new Date(ts * 1000);
  return d.toTimeString().slice(0, 8);
}

export function truncCwd(cwd) {
  if (!cwd) return "—";
  const home = "/Users/";
  if (cwd.startsWith(home)) {
    const rest = cwd.slice(home.length);
    const i = rest.indexOf("/");
    if (i >= 0) return "~" + rest.slice(i);
  }
  return cwd;
}

export function groupColor(name) {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = ((h << 5) - h + name.charCodeAt(i)) | 0;
  return GROUP_COLORS[Math.abs(h) % GROUP_COLORS.length];
}

export function listSignature(items, state, now = Date.now()) {
  const freshBucket = Math.floor(now / 15000);
  return items
    .map((i) => [
      i.session_id, i.status, i.name, i.group || "", i.pid || 0,
      i.last_tool || "", i.hook_timestamp || 0, i.notification_message || "",
      freshIntensity(i, state.pendingAcks, now) > 0 ? freshBucket : 0,
    ].join("|"))
    .join("#") + `§${state.selectedSid || ""}§${state.query}§${state.filter}§${state.groupFilter || ""}§${state.showEnded}§${[...state.collapsedGroups].sort().join(",")}`;
}

export function filterInstances(instances, filter) {
  return instances.filter((i) => {
    if (!filter.showEnded && !i.alive) return false;
    if (filter.filter !== "all" && i.status !== filter.filter) return false;
    if (filter.groupFilter && i.group !== filter.groupFilter) return false;
    if (filter.query) {
      const q = filter.query.toLowerCase();
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
