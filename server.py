#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = ["fastapi", "uvicorn[standard]", "httpx", "websockets"]
# ///
from __future__ import annotations

import asyncio
import json
import os
import pathlib
import re
import secrets
import shlex
import signal
import subprocess
import time
from datetime import datetime
from functools import lru_cache
from typing import Any

from fastapi import FastAPI, HTTPException, WebSocket, WebSocketDisconnect, Request, Response
from fastapi.responses import FileResponse, HTMLResponse, RedirectResponse, PlainTextResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel

import summarizer
import tmux_backend

HOME = pathlib.Path.home()
CLAUDE_DIR = HOME / ".claude"
SESSIONS_DIR = CLAUDE_DIR / "sessions"
PROJECTS_DIR = CLAUDE_DIR / "projects"
GLOBAL_MCP = CLAUDE_DIR / "mcp.json"
APP_DIR = HOME / ".claude-instances-ui"
STATE_DIR = APP_DIR / "state"
GROUPS_FILE = APP_DIR / "groups.json"
NAMES_FILE = APP_DIR / "names.json"
ACKS_FILE = APP_DIR / "acks.json"
STATIC_DIR = pathlib.Path(__file__).parent / "static"

HOST = os.environ.get("CIU_HOST", "127.0.0.1")
PORT = int(os.environ.get("CIU_PORT", "7878"))
PUBLIC_BASE_URL = os.environ.get("CIU_PUBLIC_URL") or f"http://{HOST}:{PORT}"

APP_DIR.mkdir(parents=True, exist_ok=True)
STATE_DIR.mkdir(parents=True, exist_ok=True)

# ----------------------------------------------------------------------------
# Auth
# ----------------------------------------------------------------------------

TOKEN_FILE = APP_DIR / "token"


def _load_or_create_token() -> str:
    try:
        if TOKEN_FILE.exists():
            t = TOKEN_FILE.read_text().strip()
            if t:
                return t
    except Exception:
        pass
    t = secrets.token_urlsafe(32)
    TOKEN_FILE.write_text(t)
    try:
        TOKEN_FILE.chmod(0o600)
    except OSError:
        pass
    return t


AUTH_TOKEN = _load_or_create_token()
COOKIE_NAME = "ciu_token"


def _is_authenticated(request: Request) -> bool:
    auth = request.headers.get("Authorization") or ""
    if auth.startswith("Bearer ") and secrets.compare_digest(auth[7:], AUTH_TOKEN):
        return True
    cookie = request.cookies.get(COOKIE_NAME) or ""
    if cookie and secrets.compare_digest(cookie, AUTH_TOKEN):
        return True
    return False


def reclassify_notification(msg: str | None) -> str:
    m = (msg or "").lower()
    if any(k in m for k in ("permission", "approval", "approve", "confirm")):
        return "needs_input"
    if "waiting for your input" in m:
        return "idle"
    return "needs_input" if m else "idle"


app = FastAPI()


def pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


def read_json(path: pathlib.Path) -> dict:
    try:
        return json.loads(path.read_text())
    except Exception:
        return {}


def write_json(path: pathlib.Path, data: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2))


_PS_CACHE: dict = {"at": 0.0, "map": {}}
_PS_TTL = 1.5


def _refresh_ps_cache() -> dict[int, dict]:
    now = time.time()
    if now - _PS_CACHE["at"] < _PS_TTL:
        return _PS_CACHE["map"]
    mapping: dict[int, dict] = {}
    try:
        out = subprocess.check_output(
            ["ps", "-ax", "-o", "pid=,ppid=,tty=,command="],
            text=True,
            stderr=subprocess.DEVNULL,
        )
        for line in out.strip().splitlines():
            parts = line.split(None, 3)
            if len(parts) < 3:
                continue
            try:
                pid_val = int(parts[0])
            except ValueError:
                continue
            mapping[pid_val] = {
                "ppid": int(parts[1]) if parts[1].isdigit() else 0,
                "tty": parts[2] if len(parts) > 2 else "",
                "command": parts[3] if len(parts) > 3 else "",
            }
    except Exception:
        return _PS_CACHE.get("map", {})
    _PS_CACHE["at"] = now
    _PS_CACHE["map"] = mapping
    return mapping


def ps_row(pid: int) -> dict:
    return _refresh_ps_cache().get(pid, {})


_TTY_CACHE: dict[int, tuple[float, list[str]]] = {}
_TTY_TTL = 5.0


def walk_ttys(pid: int, max_depth: int = 10) -> list[str]:
    cached = _TTY_CACHE.get(pid)
    if cached and time.time() - cached[0] < _TTY_TTL:
        return cached[1]
    ps_map = _refresh_ps_cache()
    seen: list[str] = []
    current = pid
    for _ in range(max_depth):
        info = ps_map.get(current)
        if not info:
            break
        tty = info.get("tty", "")
        if tty and tty != "??" and tty not in seen:
            seen.append(tty)
        current = info.get("ppid", 0)
        if current in (0, 1):
            break
    _TTY_CACHE[pid] = (time.time(), seen)
    return seen


def parse_mcp_arg(command: str) -> list[str]:
    try:
        tokens = shlex.split(command)
    except ValueError:
        return []
    paths: list[str] = []
    i = 0
    while i < len(tokens):
        t = tokens[i]
        if t == "--mcp-config" and i + 1 < len(tokens):
            paths.append(tokens[i + 1])
            i += 2
            continue
        if t.startswith("--mcp-config="):
            paths.append(t.split("=", 1)[1])
        i += 1
    return paths


def load_mcps(cwd: str | None, command: str) -> dict[str, list[str]]:
    sources: dict[str, list[str]] = {"global": [], "project": [], "explicit": []}
    if GLOBAL_MCP.exists():
        sources["global"] = sorted((read_json(GLOBAL_MCP).get("mcpServers") or {}).keys())
    if cwd:
        proj = pathlib.Path(cwd) / ".mcp.json"
        if proj.exists():
            sources["project"] = sorted((read_json(proj).get("mcpServers") or {}).keys())
    for path in parse_mcp_arg(command):
        p = pathlib.Path(path).expanduser()
        if p.exists():
            sources["explicit"].extend(sorted((read_json(p).get("mcpServers") or {}).keys()))
    sources["explicit"] = sorted(set(sources["explicit"]))
    return sources


def transcript_path_for(session_id: str, cwd: str | None) -> pathlib.Path | None:
    if not cwd:
        return None
    slug = "-" + cwd.replace("/", "-").lstrip("-")
    p = PROJECTS_DIR / slug / f"{session_id}.jsonl"
    return p if p.exists() else None


@lru_cache(maxsize=256)
def _title_cached(path_str: str, mtime: float) -> tuple[str | None, str | None]:
    title: str | None = None
    first_user: str | None = None
    try:
        with open(path_str, "r", encoding="utf-8", errors="ignore") as fh:
            for idx, line in enumerate(fh):
                if idx > 1000 and title and first_user:
                    break
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                t = d.get("type")
                if t in ("custom-title",):
                    title = d.get("customTitle") or title
                elif t == "agent-name" and not title:
                    title = d.get("agentName") or title
                elif t == "user" and not first_user:
                    msg = d.get("message") or {}
                    content = msg.get("content") if isinstance(msg, dict) else None
                    if isinstance(content, str):
                        first_user = content
                    elif isinstance(content, list):
                        for item in content:
                            if isinstance(item, dict) and item.get("type") == "text":
                                first_user = item.get("text")
                                break
    except Exception:
        pass
    if first_user:
        first_user = first_user.strip().replace("\n", " ")
        if len(first_user) > 80:
            first_user = first_user[:77] + "…"
    return title, first_user


def session_title(session_id: str, cwd: str | None) -> tuple[str | None, str | None]:
    path = transcript_path_for(session_id, cwd)
    if not path:
        return None, None
    try:
        return _title_cached(str(path), path.stat().st_mtime)
    except Exception:
        return None, None


def _iso_to_epoch(iso: str | None) -> float | None:
    if not iso:
        return None
    try:
        return datetime.fromisoformat(iso.replace("Z", "+00:00")).timestamp()
    except Exception:
        return None


def jsonl_tail(session_id: str, cwd: str | None) -> dict:
    path = transcript_path_for(session_id, cwd)
    if not path:
        return {}
    try:
        with path.open("rb") as f:
            f.seek(0, 2)
            size = f.tell()
            block = min(size, 262144)
            f.seek(size - block)
            data = f.read().decode("utf-8", errors="ignore")
        pending: dict[str, str | None] = {}
        pending_started_epoch: float | None = None
        last_ts_iso: str | None = None
        last_ts_epoch: float | None = None
        last_type: str | None = None
        last_assistant_preview: str | None = None
        for line in data.strip().splitlines():
            try:
                d = json.loads(line)
            except Exception:
                continue
            if d.get("isSidechain") or d.get("isMeta"):
                continue
            t = d.get("type")
            if t not in ("assistant", "user"):
                continue
            ts = d.get("timestamp")
            ts_epoch = _iso_to_epoch(ts) if ts else None
            if ts:
                last_ts_iso = ts
                last_ts_epoch = ts_epoch
                last_type = t
            msg = d.get("message") or {}
            content = msg.get("content") if isinstance(msg, dict) else None

            if t == "user":
                has_tool_result = False
                has_text = False
                if isinstance(content, str) and content.strip():
                    has_text = True
                elif isinstance(content, list):
                    for item in content:
                        if not isinstance(item, dict):
                            continue
                        ik = item.get("type")
                        if ik == "tool_result":
                            has_tool_result = True
                            tid = item.get("tool_use_id")
                            if tid:
                                pending.pop(tid, None)
                        elif ik == "text":
                            if (item.get("text") or "").strip():
                                has_text = True
                if has_text and not has_tool_result:
                    pending.clear()
                    pending_started_epoch = None
                elif not pending:
                    pending_started_epoch = None
            elif t == "assistant" and isinstance(content, list):
                for item in content:
                    if not isinstance(item, dict):
                        continue
                    ik = item.get("type")
                    if ik == "tool_use":
                        tid = item.get("id")
                        if tid:
                            if not pending:
                                pending_started_epoch = ts_epoch
                            pending[tid] = item.get("name")
                    elif ik == "text":
                        txt = (item.get("text") or "").strip().replace("\n", " ")
                        if txt:
                            last_assistant_preview = txt[:140] + ("…" if len(txt) > 140 else "")
        pending_tool = list(pending.values())[-1] if pending else None
        return {
            "last_type": last_type,
            "last_timestamp": last_ts_iso,
            "last_timestamp_epoch": last_ts_epoch,
            "last_assistant_preview": last_assistant_preview,
            "pending": bool(pending),
            "pending_tool": pending_tool,
            "pending_started_epoch": pending_started_epoch,
        }
    except Exception:
        return {}


def _summarize_tool_arg(tool: str | None, inp: Any) -> str:
    if not isinstance(inp, dict):
        return ""
    if tool == "Bash":
        cmd = (inp.get("command") or "").strip().replace("\n", " ")
        return cmd[:120] + ("…" if len(cmd) > 120 else "")
    if tool in ("Read", "Edit", "Write", "NotebookEdit"):
        return pathlib.Path(inp.get("file_path") or "").name or (inp.get("file_path") or "")
    if tool == "Grep":
        pat = inp.get("pattern") or ""
        where = inp.get("path") or inp.get("glob") or ""
        return f'"{pat}"' + (f" in {where}" if where else "")
    if tool == "Glob":
        return inp.get("pattern") or ""
    if tool == "WebFetch":
        return inp.get("url") or ""
    if tool == "WebSearch":
        return f'"{inp.get("query") or ""}"'
    if tool in ("Task", "Agent"):
        desc = inp.get("description") or inp.get("subagent_type") or ""
        return desc[:80]
    if tool == "TodoWrite":
        todos = inp.get("todos") or []
        return f"{len(todos)} todos"
    return ""


@lru_cache(maxsize=256)
def _summary_cached(path_str: str, mtime: float) -> dict:
    goal: str | None = None
    actions: list[dict] = []
    all_actions_count = 0
    last_text: str | None = None
    last_assistant_ts: str | None = None
    user_prompts: list[str] = []
    try:
        with open(path_str, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            block = min(size, 524288)
            f.seek(size - block)
            data = f.read().decode("utf-8", errors="ignore")
        for line in data.strip().splitlines():
            try:
                d = json.loads(line)
            except Exception:
                continue
            if d.get("isSidechain") or d.get("isMeta"):
                continue
            t = d.get("type")
            if t not in ("user", "assistant"):
                continue
            msg = d.get("message") or {}
            content = msg.get("content") if isinstance(msg, dict) else None
            ts = d.get("timestamp")
            if t == "user":
                text_parts: list[str] = []
                has_tool_result = False
                if isinstance(content, str) and content.strip():
                    text_parts.append(content)
                elif isinstance(content, list):
                    for item in content:
                        if not isinstance(item, dict):
                            continue
                        ik = item.get("type")
                        if ik == "tool_result":
                            has_tool_result = True
                        elif ik == "text":
                            txt = item.get("text") or ""
                            if txt.strip():
                                text_parts.append(txt)
                if text_parts and not has_tool_result:
                    joined = " ".join(text_parts).strip().replace("\n", " ")
                    head = joined.lstrip()[:40].lower()
                    is_meta = head.startswith("<command-") or head.startswith("<local-command") or head.startswith("[request interrupted") or head.startswith("caveat:")
                    if not is_meta:
                        goal = joined[:180] + ("…" if len(joined) > 180 else "")
                        user_prompts.append(joined[:500])
                        actions = []
                        last_text = None
            elif t == "assistant" and isinstance(content, list):
                for item in content:
                    if not isinstance(item, dict):
                        continue
                    ik = item.get("type")
                    if ik == "tool_use":
                        all_actions_count += 1
                        tool = item.get("name")
                        arg = _summarize_tool_arg(tool, item.get("input"))
                        if actions and actions[-1]["tool"] == tool and actions[-1]["arg"] == arg:
                            continue
                        actions.append({"tool": tool, "arg": arg, "ts": ts})
                        if len(actions) > 14:
                            actions = actions[-14:]
                    elif ik == "text":
                        txt = (item.get("text") or "").strip()
                        if txt:
                            last_text = txt[:400] + ("…" if len(txt) > 400 else "")
                            last_assistant_ts = ts
    except Exception:
        pass
    return {
        "goal": goal,
        "actions": actions[-8:],
        "last_text": last_text,
        "last_assistant_timestamp": last_assistant_ts,
        "prompt_count": len(user_prompts),
        "action_count": all_actions_count,
        "recent_prompts": user_prompts[-5:],
    }


def jsonl_summary(session_id: str, cwd: str | None) -> dict:
    path = transcript_path_for(session_id, cwd)
    empty = {
        "goal": None, "actions": [], "last_text": None,
        "last_assistant_timestamp": None, "paragraph": None, "paragraph_updated_at": None,
    }
    if not path:
        return empty
    try:
        mtime = path.stat().st_mtime
        base = _summary_cached(str(path), mtime)
        cached = summarizer.load(session_id)
        base = dict(base)
        base["paragraph"] = cached.get("paragraph")
        base["paragraph_updated_at"] = cached.get("updated_at")
        prev_prompts = cached.get("prompts_seen") or 0
        new_prompts = base.get("recent_prompts") or []
        if prev_prompts and len(new_prompts) > 0:
            delta = max(0, base.get("prompt_count", 0) - prev_prompts)
            new_prompts = new_prompts[-delta:] if delta else []
        summarizer.request(session_id, {
            "mtime": mtime,
            "goal": base.get("goal"),
            "actions": base.get("actions"),
            "last_text": base.get("last_text"),
            "new_prompts": new_prompts,
            "prompt_count": base.get("prompt_count"),
            "action_count": base.get("action_count"),
        })
        return base
    except Exception:
        return empty


def display_name(
    session_id: str,
    cwd: str | None,
    jsonl_title: str | None,
    names: dict,
) -> str:
    if session_id in names and names[session_id]:
        return names[session_id]
    if jsonl_title:
        return jsonl_title
    base = pathlib.Path(cwd).name if cwd else "unknown"
    return f"{base} · {session_id[:8]}"


STOP_FRESH = 10.0
NOTIFICATION_TTL = 1800.0
RUNNING_HOOK_FRESH = 60.0
GENERATION_MAX = 90.0
PENDING_EXEC_MAX = 120.0
APPROVAL_KEYWORDS = ("permission", "approval", "approve", "confirm", "allow")
RUNNING_HOOKS = {"PreToolUse", "PostToolUse", "UserPromptSubmit", "SubagentStop"}


def _is_approval_message(msg: str) -> bool:
    lower = (msg or "").lower()
    if "waiting for your input" in lower:
        return False
    return any(k in lower for k in APPROVAL_KEYWORDS)


def resolve_status(
    hook_state: dict, alive: bool, jsonl: dict
) -> tuple[str, str | None]:
    if not alive:
        return "ended", None
    now = time.time()
    hook_ts = hook_state.get("timestamp") or 0
    hook_age = now - hook_ts if hook_ts else float("inf")
    hook_event = hook_state.get("last_event")
    hook_msg = hook_state.get("notification_message") or ""

    jsonl_epoch = jsonl.get("last_timestamp_epoch")
    jsonl_age = now - jsonl_epoch if jsonl_epoch else float("inf")
    pending = bool(jsonl.get("pending"))
    pending_tool = jsonl.get("pending_tool")
    last_type = jsonl.get("last_type")

    if hook_event == "Stop" and hook_age < STOP_FRESH:
        return "idle", None

    if hook_event in RUNNING_HOOKS and hook_age < RUNNING_HOOK_FRESH:
        return "running", None

    if (
        hook_event == "Notification"
        and hook_age < NOTIFICATION_TTL
        and _is_approval_message(hook_msg)
    ):
        label = hook_msg or (f"Awaiting approval: {pending_tool}" if pending_tool else "Needs input")
        return "needs_input", label

    if last_type == "user" and jsonl_age < GENERATION_MAX:
        return "running", None

    if pending and jsonl_age < PENDING_EXEC_MAX:
        return "running", None

    return "idle", None


_HEADLESS_FLAGS = re.compile(r"(?:^|\s)(?:-p|--print)\b")


def _subagent_info(session_id: str, cwd: str | None) -> list[dict]:
    if not cwd:
        return []
    slug = "-" + cwd.replace("/", "-").lstrip("-")
    sub_dir = PROJECTS_DIR / slug / session_id / "subagents"
    if not sub_dir.exists():
        return []
    agents: list[dict] = []
    for f in sorted(sub_dir.glob("agent-*.jsonl"), key=lambda p: p.stat().st_mtime):
        agent_id = f.stem.replace("agent-", "")
        label: str | None = None
        try:
            with f.open("r") as fh:
                for line in fh:
                    try:
                        d = json.loads(line)
                    except Exception:
                        continue
                    if d.get("type") == "user":
                        msg = d.get("message") or {}
                        content = msg.get("content") if isinstance(msg, dict) else None
                        if isinstance(content, str) and content.strip():
                            label = content.strip().replace("\n", " ")[:100]
                            break
                        if isinstance(content, list):
                            for item in content:
                                if isinstance(item, dict) and item.get("type") == "text":
                                    label = (item.get("text") or "").strip().replace("\n", " ")[:100]
                                    break
                            if label:
                                break
        except Exception:
            pass
        agents.append({
            "agent_id": agent_id,
            "label": label or agent_id[:12],
            "mtime": f.stat().st_mtime,
        })
    return agents


def _is_headless(command: str) -> bool:
    return bool(_HEADLESS_FLAGS.search(command))


def gather_instances() -> list[dict]:
    names = read_json(NAMES_FILE)
    groups = read_json(GROUPS_FILE)
    acks = read_json(ACKS_FILE)
    tmux_sessions_by_sid: dict[str, str] = {s.our_sid: s.name for s in tmux_backend.list_sessions()}
    by_session: dict[str, dict] = {}

    if SESSIONS_DIR.exists():
        for sf in SESSIONS_DIR.glob("*.json"):
            data = read_json(sf)
            pid = data.get("pid")
            sid = data.get("sessionId")
            if not pid or not sid:
                continue
            alive = pid_alive(pid)
            row = ps_row(pid) if alive else {}
            command = row.get("command", "")
            if alive and _is_headless(command):
                continue
            ttys = walk_ttys(pid) if alive else []
            cwd = data.get("cwd")
            hook_state = read_json(STATE_DIR / f"{sid}.json")
            our_sid = hook_state.get("our_sid")
            tmux_name = tmux_sessions_by_sid.get(our_sid) if our_sid else None
            jsonl_title, first_user = session_title(sid, cwd)
            tail = jsonl_tail(sid, cwd)
            summary = jsonl_summary(sid, cwd)
            mcps = load_mcps(cwd, command)
            status, notif = resolve_status(hook_state, alive, tail)
            subagents = _subagent_info(sid, cwd) if alive else []
            by_session[sid] = {
                "session_id": sid,
                "pid": pid,
                "alive": alive,
                "name": display_name(sid, cwd, jsonl_title, names),
                "title": jsonl_title,
                "custom_name": names.get(sid),
                "first_user": first_user,
                "cwd": cwd,
                "kind": data.get("kind"),
                "started_at": data.get("startedAt"),
                "tty": ttys[0] if ttys else None,
                "ttys": ttys,
                "command": command,
                "status": status,
                "last_event": hook_state.get("last_event"),
                "last_tool": hook_state.get("last_tool") if status == "running" else None,
                "notification_message": notif,
                "hook_timestamp": hook_state.get("timestamp"),
                "transcript": tail,
                "summary": summary,
                "mcps": mcps,
                "subagents": subagents,
                "group": next((g for g, ids in groups.items() if sid in ids), None),
                "ack_timestamp": acks.get(sid) or 0,
                "our_sid": our_sid,
                "tmux_session": tmux_name,
            }

    now_ts = time.time()
    orphan_count = 0
    for sf in STATE_DIR.glob("*.json"):
        sid = sf.stem
        if sid in by_session:
            continue
        if orphan_count >= 200:
            break
        data = read_json(sf)
        ts = data.get("timestamp") or 0
        if now_ts - ts > 86400:
            continue
        tp = data.get("transcript_path") or ""
        if "/subagents/" in tp:
            continue
        orphan_count += 1
        cwd = data.get("cwd")
        jsonl_title, first_user = session_title(sid, cwd)
        our_sid = data.get("our_sid")
        tmux_name = tmux_sessions_by_sid.get(our_sid) if our_sid else None
        by_session[sid] = {
            "session_id": sid,
            "pid": None,
            "alive": False,
            "name": display_name(sid, cwd, jsonl_title, names),
            "title": jsonl_title,
            "custom_name": names.get(sid),
            "first_user": first_user,
            "cwd": cwd,
            "status": "ended",
            "last_event": data.get("last_event"),
            "last_tool": None,
            "notification_message": None,
            "hook_timestamp": data.get("timestamp"),
            "transcript": {},
            "summary": jsonl_summary(sid, cwd),
            "mcps": load_mcps(cwd, ""),
            "subagents": [],
            "group": next((g for g, ids in groups.items() if sid in ids), None),
            "ack_timestamp": acks.get(sid) or 0,
            "ttys": [],
            "our_sid": our_sid,
            "tmux_session": tmux_name,
        }

    def _sort_key(x: dict) -> tuple:
        alive = x["alive"]
        hook_ts = x.get("hook_timestamp") or 0
        jsonl_ts = (x.get("transcript") or {}).get("last_timestamp_epoch") or 0
        latest = max(hook_ts, jsonl_ts)
        return (not alive, -latest)

    items = sorted(by_session.values(), key=_sort_key)
    return items


class RenameBody(BaseModel):
    name: str


class GroupBody(BaseModel):
    group: str | None


class KillBody(BaseModel):
    signal: str = "TERM"


_INSTANCES_CACHE: dict = {"at": 0.0, "data": []}
_INSTANCES_TTL = 1.5


def cached_instances() -> list[dict]:
    now = time.time()
    if now - _INSTANCES_CACHE["at"] < _INSTANCES_TTL:
        return _INSTANCES_CACHE["data"]
    data = gather_instances()
    _INSTANCES_CACHE["at"] = now
    _INSTANCES_CACHE["data"] = data
    return data


@app.get("/api/instances")
def api_instances() -> dict:
    return {"instances": cached_instances(), "served_at": time.time()}


@app.get("/api/groups")
def api_groups() -> dict:
    return read_json(GROUPS_FILE)


@app.put("/api/groups")
def api_set_groups(data: dict) -> dict:
    write_json(GROUPS_FILE, data)
    return {"ok": True}


@app.put("/api/instances/{session_id}/name")
def api_rename(session_id: str, body: RenameBody) -> dict:
    names = read_json(NAMES_FILE)
    if body.name.strip():
        names[session_id] = body.name.strip()
    else:
        names.pop(session_id, None)
    write_json(NAMES_FILE, names)
    return {"ok": True}


@app.put("/api/instances/{session_id}/group")
def api_set_group(session_id: str, body: GroupBody) -> dict:
    groups = read_json(GROUPS_FILE)
    for g in list(groups):
        groups[g] = [s for s in groups[g] if s != session_id]
        if not groups[g]:
            groups.pop(g)
    if body.group:
        groups.setdefault(body.group, []).append(session_id)
    write_json(GROUPS_FILE, groups)
    return {"ok": True}


@app.post("/api/instances/{session_id}/signal")
def api_signal(session_id: str, body: KillBody) -> dict:
    target = next((i for i in cached_instances() if i["session_id"] == session_id), None)
    if not target:
        raise HTTPException(status_code=404, detail="session not found")
    sig_name = body.signal.upper()
    # Prefer tmux-backed send via pane fg-process if the session is ours
    if target.get("our_sid") and target.get("tmux_session"):
        tmux_backend.send_signal(target["our_sid"], sig_name)
        return {"ok": True, "route": "tmux"}
    pid = target.get("pid")
    if not pid:
        raise HTTPException(status_code=404, detail="pid not found")
    sig = getattr(signal, f"SIG{sig_name}", None)
    if sig is None:
        raise HTTPException(status_code=400, detail="unknown signal")
    os.kill(pid, sig)
    return {"ok": True, "route": "pid"}


@app.delete("/api/instances/{session_id}")
def api_forget(session_id: str) -> dict:
    sf = STATE_DIR / f"{session_id}.json"
    if sf.exists():
        sf.unlink()
    return {"ok": True}


class NewInstanceBody(BaseModel):
    cwd: str
    command: str = "claude"


@app.post("/api/instances/new")
def api_new_instance(body: NewInstanceBody) -> dict:
    if not tmux_backend.tmux_available():
        raise HTTPException(
            status_code=503,
            detail="tmux is not installed. Run: brew install tmux",
        )
    cwd_path = pathlib.Path(body.cwd).expanduser()
    if not cwd_path.is_dir():
        raise HTTPException(status_code=400, detail=f"not a directory: {body.cwd}")
    try:
        our_sid = tmux_backend.spawn(str(cwd_path), body.command)
    except RuntimeError as e:
        raise HTTPException(status_code=500, detail=str(e))
    # Invalidate cache so new session appears on next poll
    _INSTANCES_CACHE["at"] = 0
    return {"ok": True, "our_sid": our_sid}


@app.post("/api/instances/{session_id}/kill")
def api_kill_instance(session_id: str) -> dict:
    target = next((i for i in cached_instances() if i["session_id"] == session_id), None)
    if not target:
        raise HTTPException(status_code=404, detail="session not found")
    our_sid = target.get("our_sid")
    if not our_sid:
        raise HTTPException(status_code=400, detail="session is not tmux-owned")
    tmux_backend.kill(our_sid)
    _INSTANCES_CACHE["at"] = 0
    return {"ok": True}


@app.get("/api/recent-cwds")
def api_recent_cwds() -> dict:
    """Return a list of recently-used cwds (pulled from ~/.claude/projects/ dir names
    plus current live sessions). Useful for the new-instance modal picker."""
    seen: dict[str, float] = {}
    # Current live sessions take priority
    for inst in cached_instances():
        cwd = inst.get("cwd")
        if not cwd:
            continue
        seen[cwd] = max(seen.get(cwd, 0), inst.get("hook_timestamp") or 0)
    # Claude projects dir slugs: each dir name like "-Users-lukemarini-..." decodes to a cwd
    if PROJECTS_DIR.exists():
        for p in PROJECTS_DIR.iterdir():
            if not p.is_dir():
                continue
            slug = p.name
            if not slug.startswith("-"):
                continue
            cwd = "/" + slug[1:].replace("-", "/")
            # Only keep cwds that actually exist
            if not pathlib.Path(cwd).is_dir():
                continue
            mtime = p.stat().st_mtime
            seen[cwd] = max(seen.get(cwd, 0), mtime)
    rows = sorted(seen.items(), key=lambda kv: kv[1], reverse=True)
    return {"cwds": [c for c, _ in rows[:50]]}


@app.websocket("/ws/instances/{session_id}/terminal")
async def ws_terminal(ws: WebSocket, session_id: str) -> None:
    # Manual auth: accept then validate, then close on mismatch.
    # FastAPI doesn't pass Request to ws handlers through middleware.
    token = ws.query_params.get("t") or ""
    cookie = ws.cookies.get(COOKIE_NAME) or ""
    if not (
        (token and secrets.compare_digest(token, AUTH_TOKEN))
        or (cookie and secrets.compare_digest(cookie, AUTH_TOKEN))
    ):
        await ws.close(code=4401)
        return
    target = next((i for i in cached_instances() if i["session_id"] == session_id), None)
    if not target or not target.get("our_sid"):
        await ws.close(code=4404)
        return
    our_sid = target["our_sid"]
    if not tmux_backend.session_exists(our_sid):
        await ws.close(code=4404)
        return
    await ws.accept()

    # Send initial scrollback
    snapshot = tmux_backend.capture_pane(our_sid, lines=1000)
    if snapshot:
        try:
            await ws.send_bytes(snapshot)
        except Exception:
            return

    send_queue: asyncio.Queue[bytes] = asyncio.Queue()

    async def forward_output() -> None:
        try:
            async for chunk in tmux_backend.stream_output(our_sid):
                await ws.send_bytes(chunk)
        except Exception:
            pass

    out_task = asyncio.create_task(forward_output())
    try:
        while True:
            msg = await ws.receive()
            if msg.get("type") == "websocket.disconnect":
                break
            if "bytes" in msg and msg["bytes"] is not None:
                tmux_backend.send_bytes(our_sid, msg["bytes"])
            elif "text" in msg and msg["text"] is not None:
                text = msg["text"]
                # Text frames are JSON control messages: {"type":"input"|"resize", ...}
                try:
                    payload = json.loads(text)
                except Exception:
                    # Legacy: treat as raw keystrokes
                    tmux_backend.send_bytes(our_sid, text.encode("utf-8"))
                    continue
                kind = payload.get("type")
                if kind == "input":
                    data = payload.get("data", "")
                    if isinstance(data, str) and data:
                        tmux_backend.send_bytes(our_sid, data.encode("utf-8"))
                elif kind == "resize":
                    cols = int(payload.get("cols") or 80)
                    rows = int(payload.get("rows") or 24)
                    tmux_backend.resize_pane(our_sid, cols, rows)
    except WebSocketDisconnect:
        pass
    finally:
        out_task.cancel()
        try:
            await out_task
        except Exception:
            pass


class AckBody(BaseModel):
    timestamp: float


@app.post("/api/instances/{session_id}/ack")
def api_ack(session_id: str, body: AckBody) -> dict:
    acks = read_json(ACKS_FILE)
    cur = acks.get(session_id) or 0
    if body.timestamp > cur:
        acks[session_id] = body.timestamp
        write_json(ACKS_FILE, acks)
    return {"ok": True, "ack_timestamp": acks.get(session_id) or 0}


@app.post("/api/open-dashboard")
def api_open_dashboard() -> dict:
    subprocess.Popen(["open", f"{PUBLIC_BASE_URL}/"])
    return {"ok": True}


@app.get("/api/instances/{session_id}/transcript")
def api_transcript(session_id: str, limit: int = 60) -> dict:
    inst = next((i for i in cached_instances() if i["session_id"] == session_id), None)
    if not inst:
        raise HTTPException(status_code=404, detail="not found")
    path = transcript_path_for(session_id, inst.get("cwd"))
    entries: list[dict] = []
    if path:
        try:
            with path.open("rb") as f:
                f.seek(0, 2)
                size = f.tell()
                block = min(size, 524288)
                f.seek(size - block)
                data = f.read().decode("utf-8", errors="ignore")
            for line in data.splitlines()[-(limit * 4):]:
                try:
                    d = json.loads(line)
                except Exception:
                    continue
                t = d.get("type")
                if t not in ("user", "assistant"):
                    continue
                if d.get("isSidechain"):
                    continue
                if d.get("isMeta"):
                    continue
                uuid = d.get("uuid")
                msg = d.get("message") or {}
                content = msg.get("content") if isinstance(msg, dict) else None
                parts: list[dict] = []
                if isinstance(content, str):
                    parts.append({"kind": "text", "text": content})
                elif isinstance(content, list):
                    for item in content:
                        if not isinstance(item, dict):
                            continue
                        ik = item.get("type")
                        if ik == "text":
                            parts.append({"kind": "text", "text": item.get("text", "")})
                        elif ik == "tool_use":
                            inp = item.get("input") or {}
                            parts.append({
                                "kind": "tool_use",
                                "tool": item.get("name"),
                                "input": inp,
                            })
                        elif ik == "tool_result":
                            res = item.get("content")
                            txt = ""
                            if isinstance(res, str):
                                txt = res
                            elif isinstance(res, list):
                                txt = "\n".join(
                                    r.get("text", "")
                                    for r in res
                                    if isinstance(r, dict) and r.get("type") == "text"
                                )
                            parts.append({
                                "kind": "tool_result",
                                "text": txt,
                                "is_error": bool(item.get("is_error")),
                            })
                        elif ik == "thinking":
                            parts.append({"kind": "thinking", "text": item.get("thinking", "")})
                if not parts:
                    continue
                entries.append({
                    "uuid": uuid,
                    "type": t,
                    "timestamp": d.get("timestamp"),
                    "parts": parts,
                })
            entries = entries[-limit:]
        except Exception:
            pass
    return {
        "session": {
            "session_id": inst["session_id"],
            "name": inst["name"],
            "title": inst.get("title"),
            "custom_name": inst.get("custom_name"),
            "status": inst["status"],
            "cwd": inst.get("cwd"),
            "pid": inst.get("pid"),
            "tty": inst.get("tty"),
            "group": inst.get("group"),
            "mcps": inst.get("mcps"),
            "notification_message": inst.get("notification_message"),
            "last_tool": inst.get("last_tool"),
            "hook_timestamp": inst.get("hook_timestamp"),
            "started_at": inst.get("started_at"),
            "alive": inst.get("alive"),
            "summary": inst.get("summary"),
            "subagents": inst.get("subagents") or [],
        },
        "entries": entries,
    }


@app.get("/auth")
def auth(request: Request, t: str = "") -> Response:
    """Bootstrap endpoint: visit with ?t=<token> to set the auth cookie."""
    if not t or not secrets.compare_digest(t, AUTH_TOKEN):
        return HTMLResponse(
            "<h1>Invalid token</h1>"
            "<p>Check the token printed by the server on startup "
            "or read it from <code>~/.claude-instances-ui/token</code>.</p>",
            status_code=401,
        )
    resp = RedirectResponse(url="/", status_code=302)
    resp.set_cookie(
        key=COOKIE_NAME,
        value=AUTH_TOKEN,
        httponly=True,
        samesite="lax",
        max_age=60 * 60 * 24 * 365,
    )
    return resp


@app.get("/")
def index(request: Request) -> Response:
    if not _is_authenticated(request):
        return HTMLResponse(
            """<!doctype html><html><head><meta charset="utf-8">
<title>Claude Instances — sign in</title>
<style>
body{font-family:system-ui;background:#0b0d10;color:#d6d6d6;
  display:flex;align-items:center;justify-content:center;height:100vh;margin:0}
.card{max-width:520px;padding:32px;background:#15181c;border:1px solid #222;
  border-radius:12px;box-shadow:0 20px 60px -10px rgba(0,0,0,.6)}
code{background:#0b0d10;padding:2px 6px;border-radius:4px;color:#ffbf69}
h1{margin:0 0 12px;font-size:18px}
p{line-height:1.55;color:#a9a9a9}
</style></head><body><div class="card">
<h1>Authentication required</h1>
<p>Copy the token from <code>~/.claude-instances-ui/token</code> and visit
<code>/auth?t=&lt;token&gt;</code>. The server also printed a ready-to-click URL
on startup.</p>
</div></body></html>""",
            status_code=401,
        )
    return FileResponse(STATIC_DIR / "index.html")


from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request as StarletteRequest
from starlette.responses import Response as StarletteResponse


PUBLIC_PATHS = {"/auth"}
PUBLIC_PREFIXES = ("/static/", "/ws/")  # /ws/* auths inside the handler


class AuthMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request: StarletteRequest, call_next):
        path = request.url.path
        if path in PUBLIC_PATHS or any(path.startswith(p) for p in PUBLIC_PREFIXES):
            return await call_next(request)
        if path == "/" or _is_authenticated(request):
            return await call_next(request)
        return PlainTextResponse("unauthorized", status_code=401)


class NoCacheStaticMiddleware(BaseHTTPMiddleware):
    async def dispatch(self, request: StarletteRequest, call_next):
        response: StarletteResponse = await call_next(request)
        if request.url.path.startswith("/static/"):
            response.headers["Cache-Control"] = "no-cache, must-revalidate"
        return response


app.add_middleware(NoCacheStaticMiddleware)
app.add_middleware(AuthMiddleware)
app.mount("/static", StaticFiles(directory=STATIC_DIR), name="static")


if __name__ == "__main__":
    import uvicorn

    auth_url = f"{PUBLIC_BASE_URL}/auth?t={AUTH_TOKEN}"
    print()
    print("=" * 72)
    print("  Claude Instances UI")
    print("=" * 72)
    print(f"  Dashboard:  {PUBLIC_BASE_URL}")
    print(f"  Authenticate (first visit): {auth_url}")
    print(f"  Token file: {TOKEN_FILE}")
    print(f"  tmux:       {'available' if tmux_backend.tmux_available() else 'NOT INSTALLED — run: brew install tmux'}")
    print("=" * 72)
    print()
    uvicorn.run(app, host=HOST, port=PORT)
