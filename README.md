# Claude Instances UI

A local dashboard for monitoring multiple concurrent [Claude Code](https://claude.com/claude-code) sessions. Shows per-session status (running / needs input / idle / ended), last tool call, working directory, a rolling natural-language summary of what each agent is doing, and a menu-bar tray for at-a-glance awareness across desktops.

> **Platform**: macOS only. The project depends on iTerm2 (for terminal focus) and Hammerspoon (for the menu bar tray). See [Platform requirements](#platform-requirements).

---

## Screenshots

_(Add screenshots here after forking.)_

---

## How it works

```
 Claude Code hooks ──► hook_writer.py ──► ~/.claude-instances-ui/state/<sid>.json
                                                            │
                                                            ▼
                                                        server.py  ◄──── static/ (web UI)
                                                            │
                                                            ├──► summarizer.py (async rolling summaries)
                                                            │
                                                            └──► menubar/claude_instances.lua (Hammerspoon tray)
```

1. **`hook_writer.py`** is registered as a Claude Code hook for every session lifecycle event (SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Stop, Notification, SessionEnd, SubagentStop). It writes a tiny JSON state file per session to `~/.claude-instances-ui/state/`.
2. **`server.py`** (FastAPI) reads those state files, merges in data from Claude's JSONL transcripts (`~/.claude/projects/…`), enriches with live `ps` data and iTerm2 window titles, and serves both an HTML dashboard and a JSON API at `http://127.0.0.1:7878`.
3. **`summarizer.py`** runs as a background thread inside the server, generating 2–4 sentence rolling summaries of each session via the Anthropic API or the `claude` CLI.
4. **`menubar/claude_instances.lua`** is a Hammerspoon script that polls the server and renders a menu-bar icon + slide-in tray popover.

---

## Platform requirements

| Component | Required | Notes |
|---|---|---|
| macOS | **Yes** | AppleScript, BSD `ps`, `open`, and Hammerspoon are all macOS-only. |
| Python ≥ 3.11 | **Yes** | `server.py` uses PEP 723 inline deps — run with `uv run server.py` and deps install automatically. |
| [Claude Code](https://claude.com/claude-code) | **Yes** | Source of the hook events and transcripts this tool reads. |
| [uv](https://docs.astral.sh/uv/) | Recommended | Easiest way to run the server (`uv run server.py`). Without it, install deps manually via `pip install fastapi uvicorn[standard] httpx`. |
| [iTerm2](https://iterm2.com) | For focus feature | The "jump to terminal" button uses iTerm2 AppleScript to surface the right window/tab. Without iTerm2, everything else works; jump is a no-op. |
| [Hammerspoon](https://www.hammerspoon.org) | For menu bar | Optional. Without it, use the web UI directly at `http://127.0.0.1:7878`. |
| `ANTHROPIC_API_KEY` env var | Optional | Enables rolling summaries via the API. Falls back to the `claude` CLI if unset but available. Summaries silently disabled if neither. |

### Known non-portability

- **iTerm2-specific focus logic** (`server.py` `_ITERM_SCRIPT` and focus handler). Terminal.app, Alacritty, WezTerm, kitty, etc. are not supported.
- **BSD `ps` flag set** (`ps -ax -o pid=,ppid=,tty=,command=`). Linux `procps` has different column semantics.
- **`osascript` / AppleScript** everywhere terminal discovery happens.
- **Hammerspoon tray** is Lua + macOS-specific APIs (`hs.menubar`, `hs.webview`, `hs.eventtap`).

A port to Linux would need to replace: iTerm2 AppleScript with a supported terminal's API, `ps` parsing, and the Hammerspoon tray (e.g. with a GTK tray or a web app launcher).

---

## Install

### 1. Clone

```sh
git clone https://github.com/<your-fork>/claude-instances-ui.git
cd claude-instances-ui
```

### 2. Install the Claude Code hooks

This copies `hook_writer.py` to `~/.claude-instances-ui/bin/` and registers it in `~/.claude/settings.json` (idempotent; safe to re-run):

```sh
python3 install_hooks.py
```

> Re-run this after `git pull` to refresh the installed copy. The copy lives at a stable path so **moving or deleting the repo will not break your hooks**.

To remove them later:

```sh
python3 install_hooks.py uninstall
```

### 3. Run the server

```sh
uv run server.py
```

Or without uv:

```sh
pip install fastapi 'uvicorn[standard]' httpx
python3 server.py
```

Open [http://127.0.0.1:7878](http://127.0.0.1:7878).

### 4. (Optional) Enable the Hammerspoon menu bar tray

Install [Hammerspoon](https://www.hammerspoon.org), then copy the Lua module into your Hammerspoon config and require it:

```sh
mkdir -p ~/.hammerspoon
cp menubar/claude_instances.lua ~/.hammerspoon/claude_instances.lua
grep -qxF 'require("claude_instances")' ~/.hammerspoon/init.lua 2>/dev/null \
  || echo 'require("claude_instances")' >> ~/.hammerspoon/init.lua
```

Reload Hammerspoon (menu bar → Reload Config). A `◉` icon appears in the menu bar.

> Re-run the `cp` line after you pull updates to the repo. (Copy, not symlink, so moving the repo doesn't break the loader — mirrors how `install_hooks.py` copies `hook_writer.py` to a stable location.)

- **Click** the icon or press `⌘⇧C` to open the slide-in tray.
- **Click anywhere outside** the tray (or press `Esc`) to close it.
- The icon changes colour/glyph: warm cream `◉` (idle) → warm orange `◉` (running) → green `✦` (reply ready) → red `⚠` (needs input).

---

## Configuration

All configuration is via environment variables. Defaults in **bold**.

| Variable | Default | Purpose |
|---|---|---|
| `CIU_HOST` | **`127.0.0.1`** | Host the FastAPI server binds to. |
| `CIU_PORT` | **`7878`** | Port the server listens on. |
| `CIU_PUBLIC_URL` | **`http://$CIU_HOST:$CIU_PORT`** | Base URL the menu bar and "open full dashboard" use. Override if the server is behind a reverse proxy. |
| `ANTHROPIC_API_KEY` | _unset_ | Enables summarizer API mode. |
| `CLAUDE_INSTANCES_SUMMARY_MODEL` | **`claude-haiku-4-5`** (API) / **`haiku`** (CLI) | Model used for rolling summaries. |
| `CLAUDE_INSTANCES_UI_EPHEMERAL` | _unset_ | Internal marker; when set, the hook writer is a no-op. Used by the summarizer to avoid recursive hook storms. |

The Hammerspoon Lua module honours `CIU_HOST`, `CIU_PORT`, and `CIU_PUBLIC_URL` as well (read via `os.getenv` when Hammerspoon loads — set them in your shell profile before launching Hammerspoon).

---

## State layout

Everything persistent lives under `~/.claude-instances-ui/`:

```
~/.claude-instances-ui/
├── state/<session_id>.json    # one file per session, written by hooks
├── summary/<session_id>.json  # rolling natural-language summaries
├── groups.json                # user-defined groupings
├── names.json                 # user-defined custom names
├── acks.json                  # server-side "reply acknowledged" timestamps
├── menubar_acks.json          # Hammerspoon-side dismissal state
├── hook.log                   # hook writer errors (usually empty)
└── summarizer.log             # summarizer errors / backend negotiation
```

To fully reset: stop the server, then `rm -rf ~/.claude-instances-ui/`.

---

## Limitations & known caveats

- **macOS only** (see [Platform requirements](#platform-requirements)).
- **iTerm2 required for focus**; other terminals will render but "jump to terminal" will do nothing.
- **install_hooks.py writes `~/.claude/settings.json` without a backup.** It is idempotent and only adds marker-tagged entries, but if you have unusual JSON in your settings file, back it up yourself first.
- **No authentication** on the HTTP API. It binds to localhost by default — do not expose `CIU_HOST=0.0.0.0` on an untrusted network.
- **Summarizer uses an LLM.** When `ANTHROPIC_API_KEY` is set, each session refresh incurs a small API cost (Haiku tier, ~300 tokens). Unset the env var to disable.
- **Distribution to colleagues** requires each user to run `install_hooks.py` and set up Hammerspoon separately. No packaged installer yet.

---

## Contributing

Issues and PRs welcome. Good first areas:

- **Linux/Windows port** — wrap the platform-specific calls (`_iterm_names`, `_ps_cached`, focus, Hammerspoon tray) behind a platform interface.
- **Packaged installer** (`install.sh`) that handles hooks, server autostart (launchd), and Hammerspoon wiring.
- **Additional terminal support** (Terminal.app, WezTerm, kitty).
- **Authentication** on the HTTP API (shared token) so `CIU_HOST=0.0.0.0` can be used safely across a dev VM.

---

## License

[MIT](LICENSE)
