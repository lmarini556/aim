import { describe, it, expect } from "vitest";
import {
  FRESH_WINDOW_SECONDS,
  STATUS_GLYPH,
  STATUS_TEXT,
  GROUP_COLORS,
  freshIntensity,
  formatAnsiForLog,
  summarizeAnsi,
  escapeHtml,
  relTime,
  fmtTime,
  truncCwd,
  groupColor,
  listSignature,
  filterInstances,
} from "../../static/lib/pure.js";

const NOW = 1_700_000_000_000;

const mkInst = (over = {}) => ({
  session_id: "s1",
  alive: true,
  status: "idle",
  last_event: "Stop",
  hook_timestamp: NOW / 1000 - 60,
  ack_timestamp: 0,
  name: "n",
  group: "",
  pid: 0,
  last_tool: "",
  notification_message: "",
  ...over,
});

const mkState = (over = {}) => ({
  filter: "all",
  groupFilter: null,
  query: "",
  showEnded: false,
  instances: [],
  selectedSid: null,
  collapsedGroups: new Set(),
  pendingAcks: {},
  ...over,
});

describe("constants", () => {
  it("FRESH_WINDOW_SECONDS is 300", () => {
    expect(FRESH_WINDOW_SECONDS).toBe(300);
  });
  it("STATUS_GLYPH covers all statuses", () => {
    expect(STATUS_GLYPH).toMatchObject({
      running: "▸", idle: "●", needs_input: "!", ended: "×",
    });
  });
  it("STATUS_TEXT covers all statuses", () => {
    expect(STATUS_TEXT.running).toBe("RUNNING");
    expect(STATUS_TEXT.needs_input).toBe("NEEDS INPUT");
  });
  it("GROUP_COLORS has 15 entries", () => {
    expect(GROUP_COLORS).toHaveLength(15);
  });
});

describe("freshIntensity", () => {
  it("returns 0 when instance is not alive", () => {
    expect(freshIntensity(mkInst({ alive: false }), {}, NOW)).toBe(0);
  });
  it("returns 0 when status is not idle", () => {
    expect(freshIntensity(mkInst({ status: "running" }), {}, NOW)).toBe(0);
  });
  it("returns 0 when last_event is not Stop", () => {
    expect(freshIntensity(mkInst({ last_event: "PreToolUse" }), {}, NOW)).toBe(0);
  });
  it("returns 0 when hook_timestamp missing", () => {
    expect(freshIntensity(mkInst({ hook_timestamp: 0 }), {}, NOW)).toBe(0);
  });
  it("returns 0 when server ack covers the hook", () => {
    const ts = NOW / 1000 - 10;
    expect(freshIntensity(mkInst({ hook_timestamp: ts, ack_timestamp: ts }), {}, NOW)).toBe(0);
  });
  it("returns 0 when local pending ack covers the hook", () => {
    const ts = NOW / 1000 - 10;
    const inst = mkInst({ hook_timestamp: ts });
    expect(freshIntensity(inst, { s1: ts }, NOW)).toBe(0);
  });
  it("returns 1 when hook_timestamp is in the future (age < 0)", () => {
    expect(freshIntensity(mkInst({ hook_timestamp: NOW / 1000 + 5 }), {}, NOW)).toBe(1);
  });
  it("returns 0 when age >= FRESH_WINDOW_SECONDS", () => {
    expect(freshIntensity(mkInst({ hook_timestamp: NOW / 1000 - 300 }), {}, NOW)).toBe(0);
    expect(freshIntensity(mkInst({ hook_timestamp: NOW / 1000 - 999 }), {}, NOW)).toBe(0);
  });
  it("decays linearly from 1 to 0 across the window", () => {
    const half = freshIntensity(mkInst({ hook_timestamp: NOW / 1000 - 150 }), {}, NOW);
    expect(half).toBeCloseTo(0.5, 5);
    const quarter = freshIntensity(mkInst({ hook_timestamp: NOW / 1000 - 75 }), {}, NOW);
    expect(quarter).toBeCloseTo(0.75, 5);
  });
});

describe("escapeHtml", () => {
  it("escapes all five HTML-critical chars", () => {
    expect(escapeHtml(`<a href="x">'&'</a>`))
      .toBe("&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
  });
  it("coerces non-strings via String()", () => {
    expect(escapeHtml(42)).toBe("42");
    expect(escapeHtml(null)).toBe("null");
  });
  it("returns empty string for empty input", () => {
    expect(escapeHtml("")).toBe("");
  });
});

describe("relTime", () => {
  it("returns empty for falsy ts", () => {
    expect(relTime(0)).toBe("");
    expect(relTime(null)).toBe("");
    expect(relTime(undefined)).toBe("");
  });
  it("returns 'now' for diff < 5s", () => {
    expect(relTime(NOW / 1000 - 2, NOW)).toBe("now");
  });
  it("returns seconds for diff < 60s", () => {
    expect(relTime(NOW / 1000 - 30, NOW)).toBe("30s");
  });
  it("returns minutes for diff < 3600s", () => {
    expect(relTime(NOW / 1000 - 600, NOW)).toBe("10m");
  });
  it("returns hours for diff < 86400s", () => {
    expect(relTime(NOW / 1000 - 7200, NOW)).toBe("2h");
  });
  it("returns days for diff >= 86400s", () => {
    expect(relTime(NOW / 1000 - 172800, NOW)).toBe("2d");
  });
  it("parses ISO strings", () => {
    const iso = new Date(NOW - 60_000).toISOString();
    expect(relTime(iso, NOW)).toBe("1m");
  });
});

describe("fmtTime", () => {
  it("returns empty for falsy ts", () => {
    expect(fmtTime(0)).toBe("");
    expect(fmtTime(null)).toBe("");
  });
  it("formats numeric epoch-seconds to HH:MM:SS", () => {
    const out = fmtTime(NOW / 1000);
    expect(out).toMatch(/^\d{2}:\d{2}:\d{2}$/);
  });
  it("formats ISO string to HH:MM:SS", () => {
    const out = fmtTime(new Date(NOW).toISOString());
    expect(out).toMatch(/^\d{2}:\d{2}:\d{2}$/);
  });
});

describe("truncCwd", () => {
  it("returns em-dash for falsy", () => {
    expect(truncCwd("")).toBe("—");
    expect(truncCwd(null)).toBe("—");
  });
  it("compresses /Users/<name> to ~", () => {
    expect(truncCwd("/Users/luke/projects/aim")).toBe("~/projects/aim");
  });
  it("returns bare /Users/<name> unchanged when no further slash", () => {
    expect(truncCwd("/Users/luke")).toBe("/Users/luke");
  });
  it("passes through non-/Users paths", () => {
    expect(truncCwd("/tmp/foo")).toBe("/tmp/foo");
  });
});

describe("groupColor", () => {
  it("returns a color from GROUP_COLORS", () => {
    expect(GROUP_COLORS).toContain(groupColor("abc"));
  });
  it("is deterministic per name", () => {
    expect(groupColor("abc")).toBe(groupColor("abc"));
  });
  it("handles empty string", () => {
    expect(GROUP_COLORS).toContain(groupColor(""));
  });
  it("distinguishes different names", () => {
    const a = new Set();
    for (const n of ["aa", "bb", "cc", "dd", "ee", "ff", "gg", "hh"]) a.add(groupColor(n));
    expect(a.size).toBeGreaterThan(1);
  });
});

describe("formatAnsiForLog", () => {
  it("replaces ESC with \\e", () => {
    const bytes = new Uint8Array([0x1b, 0x5b, 0x34, 0x6d]);
    expect(formatAnsiForLog(bytes)).toBe("\\e[4m");
  });
  it("turns newline into \\n + actual newline", () => {
    expect(formatAnsiForLog(new Uint8Array([0x0a]))).toBe("\\n\n");
  });
  it("turns CR into \\r", () => {
    expect(formatAnsiForLog(new Uint8Array([0x0d]))).toBe("\\r");
  });
  it("turns TAB into \\t", () => {
    expect(formatAnsiForLog(new Uint8Array([0x09]))).toBe("\\t");
  });
  it("hex-encodes other control bytes", () => {
    expect(formatAnsiForLog(new Uint8Array([0x07]))).toBe("\\x07");
    expect(formatAnsiForLog(new Uint8Array([0x7f]))).toBe("\\x7f");
  });
  it("leaves printable chars intact", () => {
    const bytes = new TextEncoder().encode("hello");
    expect(formatAnsiForLog(bytes)).toBe("hello");
  });
  it("falls back to String() when TextDecoder throws", () => {
    expect(formatAnsiForLog("raw")).toBe("raw");
    expect(formatAnsiForLog(42)).toBe("42");
  });
});

describe("summarizeAnsi", () => {
  const enc = (s) => new TextEncoder().encode(s);
  it("returns empty string for plain text", () => {
    expect(summarizeAnsi(enc("hello"))).toBe("");
  });
  it("labels RESET for bare ESC[m", () => {
    expect(summarizeAnsi(enc("\x1b[m"))).toBe("SGR[RESET]");
  });
  it("names basic attributes", () => {
    expect(summarizeAnsi(enc("\x1b[1m"))).toBe("SGR[BOLD]");
    expect(summarizeAnsi(enc("\x1b[4m"))).toBe("SGR[*UNDERLINE*]");
    expect(summarizeAnsi(enc("\x1b[7m"))).toBe("SGR[*INVERSE*]");
    expect(summarizeAnsi(enc("\x1b[53m"))).toBe("SGR[*OVERLINE*]");
  });
  it("names basic fg/bg colors", () => {
    expect(summarizeAnsi(enc("\x1b[31m"))).toBe("SGR[fg1]");
    expect(summarizeAnsi(enc("\x1b[41m"))).toBe("SGR[*bg1*]");
    expect(summarizeAnsi(enc("\x1b[91m"))).toBe("SGR[fg1+bright]");
    expect(summarizeAnsi(enc("\x1b[101m"))).toBe("SGR[*bg1+bright*]");
  });
  it("parses 256-color fg and bg", () => {
    expect(summarizeAnsi(enc("\x1b[38;5;42m"))).toBe("SGR[fg256:42]");
    expect(summarizeAnsi(enc("\x1b[48;5;42m"))).toBe("SGR[*bg256:42*]");
  });
  it("parses truecolor fg and bg", () => {
    expect(summarizeAnsi(enc("\x1b[38;2;10;20;30m"))).toBe("SGR[fgRGB:10,20,30]");
    expect(summarizeAnsi(enc("\x1b[48;2;10;20;30m"))).toBe("SGR[*bgRGB:10,20,30*]");
  });
  it("labels unknown params with pN", () => {
    expect(summarizeAnsi(enc("\x1b[99m"))).toBe("SGR[p99]");
  });
  it("detects block glyphs", () => {
    expect(summarizeAnsi(enc("▀▀█"))).toBe("GLYPHS:▀█×3");
  });
  it("combines multiple SGR chunks and glyphs", () => {
    const out = summarizeAnsi(enc("\x1b[1m\x1b[31m▄"));
    expect(out).toContain("SGR[BOLD]");
    expect(out).toContain("SGR[fg1]");
    expect(out).toContain("GLYPHS:▄×1");
  });
  it("names all attribute codes (2, 3, 9)", () => {
    expect(summarizeAnsi(enc("\x1b[2m"))).toBe("SGR[DIM]");
    expect(summarizeAnsi(enc("\x1b[3m"))).toBe("SGR[ITALIC]");
    expect(summarizeAnsi(enc("\x1b[9m"))).toBe("SGR[STRIKE]");
  });
  it("names RESET for explicit 0 param", () => {
    expect(summarizeAnsi(enc("\x1b[0m"))).toBe("SGR[RESET]");
  });
  it("returns empty string when TextDecoder throws", () => {
    expect(summarizeAnsi("raw")).toBe("");
  });
});

describe("filterInstances", () => {
  const items = [
    { session_id: "a", alive: true, status: "running", name: "alpha", cwd: "/Users/x/work", group: "g1", mcps: { global: ["filesystem"] } },
    { session_id: "b", alive: false, status: "ended", name: "beta", cwd: "/tmp", group: "g2", mcps: {} },
    { session_id: "c", alive: true, status: "idle", name: "gamma", cwd: "/tmp", group: "g1", mcps: { project: ["context7"] } },
  ];
  it("hides ended by default", () => {
    const out = filterInstances(items, mkState());
    expect(out.map((i) => i.session_id)).toEqual(["a", "c"]);
  });
  it("shows ended when showEnded=true", () => {
    const out = filterInstances(items, mkState({ showEnded: true }));
    expect(out).toHaveLength(3);
  });
  it("filters by status", () => {
    expect(filterInstances(items, mkState({ filter: "running" })).map((i) => i.session_id)).toEqual(["a"]);
    expect(filterInstances(items, mkState({ filter: "idle" })).map((i) => i.session_id)).toEqual(["c"]);
  });
  it("filters by group", () => {
    const out = filterInstances(items, mkState({ groupFilter: "g1" }));
    expect(out.map((i) => i.session_id)).toEqual(["a", "c"]);
  });
  it("group filter excludes items in other groups", () => {
    const out = filterInstances(items, mkState({ groupFilter: "g2", showEnded: true }));
    expect(out.map((i) => i.session_id)).toEqual(["b"]);
  });
  it("filters by query over name/cwd/group/mcps", () => {
    expect(filterInstances(items, mkState({ query: "alpha" })).map((i) => i.session_id)).toEqual(["a"]);
    expect(filterInstances(items, mkState({ query: "FILESYSTEM" })).map((i) => i.session_id)).toEqual(["a"]);
    expect(filterInstances(items, mkState({ query: "context7" })).map((i) => i.session_id)).toEqual(["c"]);
    expect(filterInstances(items, mkState({ query: "g2", showEnded: true })).map((i) => i.session_id)).toEqual(["b"]);
  });
  it("query is ANDed with other filters", () => {
    const out = filterInstances(items, mkState({ filter: "idle", query: "gamma" }));
    expect(out.map((i) => i.session_id)).toEqual(["c"]);
  });
  it("tolerates missing mcps fields", () => {
    const out = filterInstances([{ session_id: "z", alive: true, status: "running", name: "z" }], mkState({ query: "z" }));
    expect(out).toHaveLength(1);
  });
  it("searches across global, project, and explicit mcp arrays", () => {
    const full = [{
      session_id: "a", alive: true, status: "running", name: "n", cwd: "", group: "",
      mcps: { global: ["ga"], project: ["pb"], explicit: ["ec"] },
    }];
    expect(filterInstances(full, mkState({ query: "ga" }))).toHaveLength(1);
    expect(filterInstances(full, mkState({ query: "pb" }))).toHaveLength(1);
    expect(filterInstances(full, mkState({ query: "ec" }))).toHaveLength(1);
    expect(filterInstances(full, mkState({ query: "zz" }))).toHaveLength(0);
  });
});

describe("listSignature", () => {
  it("embeds state-level filter selections into the signature", () => {
    const s1 = listSignature([], mkState({ filter: "idle" }), NOW);
    const s2 = listSignature([], mkState({ filter: "running" }), NOW);
    expect(s1).not.toBe(s2);
  });
  it("changes when selectedSid changes", () => {
    const s1 = listSignature([], mkState({ selectedSid: "a" }), NOW);
    const s2 = listSignature([], mkState({ selectedSid: "b" }), NOW);
    expect(s1).not.toBe(s2);
  });
  it("changes when collapsedGroups changes", () => {
    const s1 = listSignature([], mkState({ collapsedGroups: new Set(["g1"]) }), NOW);
    const s2 = listSignature([], mkState({ collapsedGroups: new Set(["g2"]) }), NOW);
    expect(s1).not.toBe(s2);
  });
  it("includes fresh bucket only when item is fresh", () => {
    const stale = mkInst({ hook_timestamp: NOW / 1000 - 999 });
    const fresh = mkInst({ hook_timestamp: NOW / 1000 - 10 });
    const s1 = listSignature([stale], mkState(), NOW);
    const s2 = listSignature([fresh], mkState(), NOW);
    expect(s1).not.toBe(s2);
  });
  it("is stable across identical inputs", () => {
    const items = [mkInst({ session_id: "a" })];
    expect(listSignature(items, mkState(), NOW)).toBe(listSignature(items, mkState(), NOW));
  });
  it("falls back to empty strings and zeros for missing optional fields", () => {
    const sparse = [{ session_id: "a", alive: true, status: "idle", name: "n" }];
    expect(() => listSignature(sparse, mkState(), NOW)).not.toThrow();
  });
  it("includes all collapsedGroups in sorted order", () => {
    const a = listSignature([], mkState({ collapsedGroups: new Set(["b", "a"]) }), NOW);
    const b = listSignature([], mkState({ collapsedGroups: new Set(["a", "b"]) }), NOW);
    expect(a).toBe(b);
  });
});
