"""tmux-backed session lifecycle + I/O.

Uses a dedicated tmux socket (`-L ciu`) so our sessions never collide with a
user's own tmux server. Sessions are named `ciu-<ulid>`. Output is streamed
via `tmux pipe-pane` into a per-session FIFO; input is injected via
`tmux send-keys -H <hex>`.

The module is import-safe even when tmux isn't installed — `tmux_available()`
returns False and callers are expected to degrade gracefully.
"""
from __future__ import annotations

import asyncio
import os
import pathlib
import secrets
import shutil
import subprocess
import time
from dataclasses import dataclass
from typing import AsyncIterator

HOME = pathlib.Path.home()
APP_DIR = HOME / ".claude-instances-ui"
FIFO_DIR = APP_DIR / "tmux"
FIFO_DIR.mkdir(parents=True, exist_ok=True)

SOCKET_NAME = "ciu"
NAME_PREFIX = "ciu-"
TMUX_BIN = shutil.which("tmux")


def tmux_available() -> bool:
    return TMUX_BIN is not None


def _tmux(*args: str, check: bool = False, **kw) -> subprocess.CompletedProcess:
    if not TMUX_BIN:
        raise RuntimeError("tmux not installed")
    cmd = [TMUX_BIN, "-L", SOCKET_NAME, *args]
    return subprocess.run(cmd, capture_output=True, text=True, check=check, **kw)


def _new_our_sid() -> str:
    # Short unguessable id; collision-resistant and URL-safe.
    return secrets.token_hex(6)


def _session_name(our_sid: str) -> str:
    return f"{NAME_PREFIX}{our_sid}"


def _fifo_path(our_sid: str) -> pathlib.Path:
    return FIFO_DIR / f"{our_sid}.fifo"


@dataclass
class TmuxSession:
    our_sid: str
    name: str
    created_at: float
    cwd: str


def list_sessions() -> list[TmuxSession]:
    if not tmux_available():
        return []
    res = _tmux(
        "list-sessions",
        "-F",
        "#{session_name}|#{session_created}|#{pane_current_path}",
    )
    if res.returncode != 0:
        return []
    out: list[TmuxSession] = []
    for line in res.stdout.splitlines():
        parts = line.split("|", 2)
        if len(parts) < 3:
            continue
        name, created, cwd = parts
        if not name.startswith(NAME_PREFIX):
            continue
        try:
            created_f = float(created)
        except ValueError:
            created_f = time.time()
        out.append(
            TmuxSession(
                our_sid=name[len(NAME_PREFIX):],
                name=name,
                created_at=created_f,
                cwd=cwd,
            )
        )
    return out


def session_exists(our_sid: str) -> bool:
    res = _tmux("has-session", "-t", _session_name(our_sid))
    return res.returncode == 0


def spawn(cwd: str, command: str = "claude", extra_env: dict[str, str] | None = None) -> str:
    """Create a new tmux session running `command` in `cwd`.

    Returns the our_sid. The tmux session will be named ciu-<our_sid>. An env
    var CLAUDE_INSTANCES_UI_OWNED=<our_sid> is injected so the Claude hook
    writer can stamp it into the state file for correlation.
    """
    if not tmux_available():
        raise RuntimeError("tmux not installed — run: brew install tmux")
    our_sid = _new_our_sid()
    name = _session_name(our_sid)
    env = os.environ.copy()
    env["CLAUDE_INSTANCES_UI_OWNED"] = our_sid
    if extra_env:
        env.update(extra_env)
    # -d: detached; -s: session name; -c: cwd; quoted command as final arg
    res = subprocess.run(
        [
            TMUX_BIN, "-L", SOCKET_NAME,
            "new-session",
            "-d",
            "-s", name,
            "-c", cwd,
            "-x", "200",
            "-y", "50",
            command,
        ],
        capture_output=True,
        text=True,
        env=env,
    )
    if res.returncode != 0:
        raise RuntimeError(f"tmux new-session failed: {res.stderr.strip()}")
    # Decouple pane size from attached clients so resize-window takes effect
    _tmux("set-option", "-t", name, "window-size", "manual")
    _start_pipe(name, our_sid)
    return our_sid


def _start_pipe(session_name: str, our_sid: str) -> None:
    """Ensure a FIFO exists and pipe-pane writes to it."""
    fifo = _fifo_path(our_sid)
    try:
        if fifo.exists():
            fifo.unlink()
        os.mkfifo(fifo, mode=0o600)
    except FileExistsError:
        pass
    _tmux(
        "pipe-pane",
        "-o",  # toggle; -o forces open (new pipe replaces any prior)
        "-t",
        session_name,
        f"cat > {fifo}",
    )


def kill(our_sid: str) -> None:
    if not tmux_available():
        return
    _tmux("kill-session", "-t", _session_name(our_sid))
    fifo = _fifo_path(our_sid)
    try:
        fifo.unlink()
    except FileNotFoundError:
        pass


def send_signal(our_sid: str, sig: str = "INT") -> None:
    """Send a Unix signal to the foreground process of the session.

    `sig` is one of INT, TERM. Uses tmux's ability to find the pane PID and
    its foreground child.
    """
    if not tmux_available():
        return
    res = _tmux(
        "display-message",
        "-p",
        "-t", _session_name(our_sid),
        "#{pane_pid}",
    )
    pid_s = res.stdout.strip()
    if not pid_s.isdigit():
        return
    pane_pid = int(pid_s)
    # Find the foreground child of the pane process
    ps_res = subprocess.run(
        ["ps", "-o", "pid=,ppid=,stat=", "-ax"],
        capture_output=True,
        text=True,
    )
    target_pid = pane_pid
    for line in ps_res.stdout.splitlines():
        parts = line.split(None, 2)
        if len(parts) < 3:
            continue
        try:
            pid = int(parts[0])
            ppid = int(parts[1])
        except ValueError:
            continue
        stat = parts[2]
        # foreground child of pane: has pane as parent and "+" in stat
        if ppid == pane_pid and "+" in stat:
            target_pid = pid
            break
    sig_map = {"INT": 2, "TERM": 15}
    try:
        os.kill(target_pid, sig_map.get(sig.upper(), 2))
    except ProcessLookupError:
        pass


def capture_pane(our_sid: str, lines: int = 2000) -> bytes:
    """Return the scrollback + current screen as escape-preserving bytes."""
    if not tmux_available():
        return b""
    res = _tmux(
        "capture-pane",
        "-p",       # print to stdout
        "-e",       # include escape sequences
        "-J",       # join wrapped lines... actually want to preserve visible wrap
        "-S", f"-{lines}",
        "-t", _session_name(our_sid),
    )
    if res.returncode != 0:
        return b""
    return res.stdout.encode("utf-8", errors="replace")


def send_bytes(our_sid: str, data: bytes) -> None:
    """Inject raw bytes into the session as if typed."""
    if not tmux_available() or not data:
        return
    # Chunk into manageable arg-list sizes
    CHUNK = 512
    for i in range(0, len(data), CHUNK):
        chunk = data[i:i + CHUNK]
        hex_pairs = [f"0x{b:02x}" for b in chunk]
        subprocess.run(
            [TMUX_BIN, "-L", SOCKET_NAME, "send-keys", "-t", _session_name(our_sid), "-H", *hex_pairs],
            capture_output=True,
        )


def resize_pane(our_sid: str, cols: int, rows: int) -> None:
    if not tmux_available():
        return
    name = _session_name(our_sid)
    try:
        _tmux("resize-window", "-t", name, "-x", str(max(1, cols)), "-y", str(max(1, rows)))
    except Exception:
        pass


async def stream_output(our_sid: str) -> AsyncIterator[bytes]:
    """Async-iterate over bytes written by the session since stream start.

    Re-opens the FIFO if the session was re-piped. Yields bytes as they arrive.
    """
    if not tmux_available():
        return
    fifo = _fifo_path(our_sid)
    # Ensure pipe is active
    _start_pipe(_session_name(our_sid), our_sid)

    loop = asyncio.get_event_loop()

    def _open_nonblocking() -> int:
        # Open read-only + nonblocking so open() returns even if no writer yet
        return os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)

    fd = await loop.run_in_executor(None, _open_nonblocking)
    try:
        reader = asyncio.StreamReader(loop=loop)
        protocol = asyncio.StreamReaderProtocol(reader, loop=loop)
        transport, _ = await loop.connect_read_pipe(lambda: protocol, os.fdopen(fd, "rb", buffering=0))
        try:
            while True:
                chunk = await reader.read(4096)
                if not chunk:
                    await asyncio.sleep(0.05)
                    continue
                yield chunk
        finally:
            transport.close()
    except Exception:
        try:
            os.close(fd)
        except OSError:
            pass
        raise
