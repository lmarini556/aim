#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import pathlib
import sys
import time
import traceback

STATE_DIR = pathlib.Path.home() / ".claude-instances-ui" / "state"
LOG_FILE = pathlib.Path.home() / ".claude-instances-ui" / "hook.log"
STATUS = {
    "SessionStart": "idle",
    "UserPromptSubmit": "running",
    "PreToolUse": "running",
    "PostToolUse": "running",
    "SubagentStop": "running",
    "Stop": "idle",
    "SessionEnd": "ended",
}

APPROVAL_KEYWORDS = ("permission", "approval", "approve", "confirm", "allow")


def classify_notification(message):
    msg = (message or "").lower()
    if "waiting for your input" in msg:
        return "idle"
    if any(k in msg for k in APPROVAL_KEYWORDS):
        return "needs_input"
    if not msg:
        return "needs_input"
    return "needs_input"


def safe_main():
    if os.environ.get("CLAUDE_INSTANCES_UI_EPHEMERAL"):
        return

    try:
        STATE_DIR.mkdir(parents=True, exist_ok=True)
    except Exception:
        return

    raw = ""
    try:
        if not sys.stdin.isatty():
            raw = sys.stdin.read()
    except Exception:
        raw = ""

    try:
        payload = json.loads(raw) if raw else {}
    except Exception:
        payload = {}

    event = payload.get("hook_event_name") or (sys.argv[1] if len(sys.argv) > 1 else "unknown")
    session_id = payload.get("session_id")
    if not session_id:
        return

    state_file = STATE_DIR / f"{session_id}.json"
    existing = {}
    if state_file.exists():
        try:
            existing = json.loads(state_file.read_text())
        except Exception:
            existing = {}

    if event == "Notification":
        status = classify_notification(payload.get("message"))
    else:
        status = STATUS.get(event, existing.get("status", "unknown"))

    if event in ("Stop", "UserPromptSubmit", "PreToolUse", "PostToolUse"):
        notification_message = None
        last_tool = payload.get("tool_name") if event in ("PreToolUse", "PostToolUse") else None
    elif event == "Notification":
        notification_message = payload.get("message")
        last_tool = existing.get("last_tool")
    else:
        notification_message = existing.get("notification_message")
        last_tool = payload.get("tool_name") or existing.get("last_tool")

    existing.update(
        {
            "session_id": session_id,
            "status": status,
            "last_event": event,
            "last_tool": last_tool,
            "cwd": payload.get("cwd") or existing.get("cwd"),
            "transcript_path": payload.get("transcript_path") or existing.get("transcript_path"),
            "notification_message": notification_message,
            "timestamp": time.time(),
        }
    )
    if event == "SessionEnd":
        existing["ended_at"] = time.time()
    try:
        state_file.write_text(json.dumps(existing))
    except Exception:
        pass


def main():
    try:
        safe_main()
    except Exception:
        try:
            with LOG_FILE.open("a") as f:
                f.write(f"--- {time.time()} ---\n")
                traceback.print_exc(file=f)
        except Exception:
            pass


if __name__ == "__main__":
    main()
