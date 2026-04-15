#!/usr/bin/env python3
import json
import pathlib
import shutil
import sys

SETTINGS = pathlib.Path.home() / ".claude" / "settings.json"
APP_DIR = pathlib.Path.home() / ".claude-instances-ui"
BIN_DIR = APP_DIR / "bin"
SOURCE_HOOK = (pathlib.Path(__file__).parent / "hook_writer.py").resolve()
INSTALLED_HOOK = BIN_DIR / "hook_writer.py"
MARKER = "# claude-instances-ui"
EVENTS = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "SubagentStop",
    "Notification",
    "Stop",
    "SessionEnd",
]


def entry() -> dict:
    return {
        "hooks": [
            {
                "type": "command",
                "command": f"/usr/bin/env python3 {INSTALLED_HOOK} {MARKER}",
            }
        ]
    }


def install() -> None:
    if not SOURCE_HOOK.exists():
        print(f"source hook not found at {SOURCE_HOOK}", file=sys.stderr)
        sys.exit(1)

    BIN_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copy2(SOURCE_HOOK, INSTALLED_HOOK)
    INSTALLED_HOOK.chmod(0o755)

    settings = {}
    if SETTINGS.exists():
        try:
            settings = json.loads(SETTINGS.read_text())
        except Exception:
            print("existing settings.json not JSON, aborting", file=sys.stderr)
            sys.exit(1)

    hooks = settings.setdefault("hooks", {})
    for event in EVENTS:
        bucket = hooks.setdefault(event, [])
        bucket[:] = [b for b in bucket if MARKER not in json.dumps(b)]
        bucket.append(entry())

    SETTINGS.parent.mkdir(parents=True, exist_ok=True)
    SETTINGS.write_text(json.dumps(settings, indent=2))
    print(f"copied hook to {INSTALLED_HOOK}")
    print(f"installed hooks in {SETTINGS}")


def uninstall() -> None:
    if SETTINGS.exists():
        try:
            settings = json.loads(SETTINGS.read_text())
        except Exception:
            settings = {}
        hooks = settings.get("hooks", {})
        for event in EVENTS:
            bucket = hooks.get(event, [])
            hooks[event] = [b for b in bucket if MARKER not in json.dumps(b)]
            if not hooks[event]:
                hooks.pop(event, None)
        if not hooks:
            settings.pop("hooks", None)
        SETTINGS.write_text(json.dumps(settings, indent=2))
        print(f"uninstalled hooks from {SETTINGS}")

    if INSTALLED_HOOK.exists():
        INSTALLED_HOOK.unlink()
        print(f"removed {INSTALLED_HOOK}")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "install"
    if cmd == "uninstall":
        uninstall()
    else:
        install()
