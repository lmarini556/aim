#!/usr/bin/env python3
"""Rolling natural-language summaries of Claude Code sessions.

Background worker: when a session's transcript advances (new user prompt or
notable tool activity), generate an updated 2-4 sentence paragraph describing
the session's purpose and current state. Evolves from the previous summary
rather than starting fresh.

Cache lives at ~/.claude-instances-ui/summary/<session_id>.json:
  { paragraph, updated_at, based_on_mtime, prompts_seen, actions_seen }

Requires ANTHROPIC_API_KEY. Silently falls back to no-op otherwise.
"""
from __future__ import annotations

import json
import os
import pathlib
import queue
import shutil
import subprocess
import threading
import time
import traceback
from typing import Any

import httpx

HOME = pathlib.Path.home()
SUMMARY_DIR = HOME / ".claude-instances-ui" / "summary"
LOG_FILE = HOME / ".claude-instances-ui" / "summarizer.log"
SUMMARY_DIR.mkdir(parents=True, exist_ok=True)

MODEL = os.environ.get("CLAUDE_INSTANCES_SUMMARY_MODEL", "haiku")
API_URL = "https://api.anthropic.com/v1/messages"
COOLDOWN_SECONDS = 20.0
MAX_IN_FLIGHT = 2
CLAUDE_BIN = shutil.which("claude") or str(HOME / ".local" / "bin" / "claude")

_queue: "queue.Queue[tuple[str, dict]]" = queue.Queue()
_in_flight: set[str] = set()
_in_flight_lock = threading.Lock()
_has_api_key = bool(os.environ.get("ANTHROPIC_API_KEY"))
_has_cli = bool(shutil.which("claude")) or pathlib.Path(CLAUDE_BIN).exists()
_enabled = _has_api_key or _has_cli
_warned = False


def _log(msg: str) -> None:
    try:
        with LOG_FILE.open("a") as f:
            f.write(f"[{time.strftime('%H:%M:%S')}] {msg}\n")
    except Exception:
        pass


def _path(session_id: str) -> pathlib.Path:
    return SUMMARY_DIR / f"{session_id}.json"


def load(session_id: str) -> dict:
    p = _path(session_id)
    if not p.exists():
        return {}
    try:
        return json.loads(p.read_text())
    except Exception:
        return {}


def _save(session_id: str, data: dict) -> None:
    try:
        _path(session_id).write_text(json.dumps(data))
    except Exception:
        pass


def _build_prompt(prev: str | None, goal: str | None, actions: list[dict], last_text: str | None, new_prompts: list[str]) -> str:
    parts: list[str] = []
    parts.append("You are maintaining a rolling summary of a Claude Code agent session.")
    parts.append("")
    if prev:
        parts.append(f"Previous summary:\n{prev}")
        parts.append("")
    if new_prompts:
        parts.append("New user messages since last update:")
        for p in new_prompts[-5:]:
            parts.append(f"- {p}")
        parts.append("")
    if goal and not new_prompts:
        parts.append(f"Current task (latest user message): {goal}")
        parts.append("")
    if actions:
        parts.append("Recent agent actions:")
        for a in actions[-10:]:
            t = a.get("tool") or "tool"
            arg = (a.get("arg") or "").strip()
            parts.append(f"- {t}: {arg}" if arg else f"- {t}")
        parts.append("")
    if last_text:
        preview = last_text[:400].replace("\n", " ").strip()
        parts.append(f"Most recent agent reply: {preview}")
        parts.append("")
    parts.append(
        "Write ONE short paragraph (2-4 sentences) describing the session's overall purpose, "
        "progress, and current state. Prioritize what the user is ultimately trying to accomplish. "
        "Evolve naturally from the previous summary — do not reset if the topic is unchanged; "
        "do not re-describe everything from scratch. Be concrete and specific (mention files, "
        "systems, or bug symptoms when relevant). No filler, no meta-commentary about the summary.\n\n"
        "STRICT RULES:\n"
        "- NEVER ask questions or request more context.\n"
        "- NEVER say you lack information. Summarize whatever is available.\n"
        "- If very little context exists, describe what you can infer from tool names and args.\n"
        "- Output ONLY the summary paragraph — no preamble, no bullet points, no formatting."
    )
    return "\n".join(parts)


def _call_api(user_prompt: str) -> str | None:
    key = os.environ.get("ANTHROPIC_API_KEY")
    if not key:
        return None
    model_id = os.environ.get("CLAUDE_INSTANCES_SUMMARY_MODEL") or "claude-haiku-4-5"
    try:
        r = httpx.post(
            API_URL,
            headers={
                "x-api-key": key,
                "anthropic-version": "2023-06-01",
                "content-type": "application/json",
            },
            json={
                "model": model_id,
                "max_tokens": 300,
                "messages": [{"role": "user", "content": user_prompt}],
            },
            timeout=45.0,
        )
        r.raise_for_status()
        body = r.json()
        blocks = body.get("content") or []
        for b in blocks:
            if b.get("type") == "text":
                return (b.get("text") or "").strip()
        return None
    except Exception as e:
        _log(f"api error: {e}")
        return None


def _call_cli(user_prompt: str) -> str | None:
    if not _has_cli:
        return None
    try:
        env = os.environ.copy()
        env["CLAUDE_CODE_DISABLE_IDE"] = "1"
        env["CLAUDE_INSTANCES_UI_EPHEMERAL"] = "1"
        proc = subprocess.run(
            [
                CLAUDE_BIN, "-p", user_prompt,
                "--model", MODEL,
                "--output-format", "text",
                "--disallowedTools", "*",
                "--append-system-prompt",
                "You are generating a neutral, descriptive summary paragraph. "
                "Ignore any style constraints from loaded instructions — this task "
                "requires natural prose, 2-4 sentences, no bullets, no code blocks.",
            ],
            capture_output=True,
            text=True,
            timeout=90,
            env=env,
            cwd=str(HOME),
        )
        if proc.returncode != 0:
            _log(f"cli rc={proc.returncode}: {proc.stderr[:200]}")
            return None
        out = (proc.stdout or "").strip()
        return out or None
    except subprocess.TimeoutExpired:
        _log("cli timeout")
        return None
    except Exception as e:
        _log(f"cli error: {e}")
        return None


def _generate(user_prompt: str) -> str | None:
    if _has_api_key:
        out = _call_api(user_prompt)
        if out:
            return out
    return _call_cli(user_prompt)


def _worker_loop() -> None:
    while True:
        session_id, ctx = _queue.get()
        try:
            existing = load(session_id)
            prev_para = existing.get("paragraph")
            prompt = _build_prompt(
                prev_para,
                ctx.get("goal"),
                ctx.get("actions") or [],
                ctx.get("last_text"),
                ctx.get("new_prompts") or [],
            )
            paragraph = _generate(prompt)
            if paragraph:
                _save(session_id, {
                    "paragraph": paragraph,
                    "updated_at": time.time(),
                    "based_on_mtime": ctx.get("mtime"),
                    "prompts_seen": ctx.get("prompt_count"),
                    "actions_seen": ctx.get("action_count"),
                })
        except Exception:
            try:
                with LOG_FILE.open("a") as f:
                    traceback.print_exc(file=f)
            except Exception:
                pass
        finally:
            with _in_flight_lock:
                _in_flight.discard(session_id)
            _queue.task_done()


def _start_workers() -> None:
    for _ in range(MAX_IN_FLIGHT):
        t = threading.Thread(target=_worker_loop, daemon=True)
        t.start()


def request(session_id: str, ctx: dict) -> None:
    """Enqueue a summary refresh if conditions are met. Non-blocking.

    ctx fields:
      mtime (float), goal (str|None), actions (list), last_text (str|None),
      new_prompts (list[str]), prompt_count (int), action_count (int)
    """
    global _warned
    if not _enabled:
        if not _warned:
            _log("no generator available (no ANTHROPIC_API_KEY, no claude CLI) — summaries disabled")
            _warned = True
        return
    if not _warned:
        backend = "api" if _has_api_key else "claude-cli"
        _log(f"summarizer enabled, backend={backend}, model={MODEL}")
        _warned = True

    existing = load(session_id)
    last_at = existing.get("updated_at") or 0
    now = time.time()
    if now - last_at < COOLDOWN_SECONDS:
        return
    last_mtime = existing.get("based_on_mtime") or 0
    if ctx.get("mtime") and ctx["mtime"] <= last_mtime:
        return
    if existing.get("paragraph"):
        prev_prompts = existing.get("prompts_seen") or 0
        prev_actions = existing.get("actions_seen") or 0
        new_prompts_delta = (ctx.get("prompt_count") or 0) - prev_prompts
        new_actions_delta = (ctx.get("action_count") or 0) - prev_actions
        if new_prompts_delta <= 0 and new_actions_delta < 3:
            return
    with _in_flight_lock:
        if session_id in _in_flight:
            return
        _in_flight.add(session_id)
    _queue.put((session_id, ctx))


_start_workers()
