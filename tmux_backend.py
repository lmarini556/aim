"""tmux-backed session lifecycle + I/O.

Uses a dedicated tmux socket (`-L ciu`) so our sessions never collide with a
user's own tmux server. Sessions are named `ciu-<ulid>`. WebSocket terminals
attach via a real pty (`pty_attach`) for bidirectional I/O — the same
architecture used by ttyd, wetty, and gotty. Legacy pipe-pane / send-keys
helpers are retained for non-interactive callers.

The module is import-safe even when tmux isn't installed — `tmux_available()`
returns False and callers are expected to degrade gracefully.
"""
from __future__ import annotations

import asyncio
import fcntl
import os
import pathlib
import pty as pty_mod
import secrets
import shutil
import struct
import subprocess
import termios
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


_global_keys_set = False


def _ensure_global_keys() -> None:
    """Set server-wide tmux options once per process lifetime."""
    global _global_keys_set
    if _global_keys_set:
        return
    _global_keys_set = True
    _tmux("set-option", "-g", "extended-keys", "always")
    _tmux("set-option", "-g", "-a", "terminal-features",
          ",xterm-256color:extkeys")
    _tmux("bind-key", "-n", "S-Enter", "send-keys", "Escape", "[13;2u")


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
    _ensure_global_keys()
    our_sid = _new_our_sid()
    name = _session_name(our_sid)
    env = os.environ.copy()
    env["CLAUDE_INSTANCES_UI_OWNED"] = our_sid
    if extra_env:
        env.update(extra_env)
    # -d: detached; -s: session name; -c: cwd; quoted command as final arg
    # Pass env explicitly via `-e` so the var reaches the pane's child even
    # when the tmux server was already running (attaching to an existing
    # server ignores the caller's env for new sessions).
    env_args: list[str] = ["-e", f"CLAUDE_INSTANCES_UI_OWNED={our_sid}"]
    if extra_env:
        for k, v in extra_env.items():
            env_args += ["-e", f"{k}={v}"]
    res = subprocess.run(
        [
            TMUX_BIN, "-L", SOCKET_NAME,
            "new-session",
            "-d",
            "-s", name,
            "-c", cwd,
            "-x", "200",
            "-y", "50",
            *env_args,
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
    # Enable mouse so scroll wheel enters copy-mode for scrollback
    _tmux("set-option", "-t", name, "mouse", "on")
    # Generous scrollback for long Claude conversations
    _tmux("set-option", "-t", name, "history-limit", "50000")
    # Keep pane alive if Claude exits so the user can see errors
    _tmux("set-option", "-t", name, "remain-on-exit", "on")
    # Extended keys so Shift+Enter and other modified keys reach Claude.
    # "always" enables CSI u forwarding without waiting for the inner app
    # to request it via the kitty keyboard protocol activation sequence.
    _tmux("set-option", "-t", name, "extended-keys", "always")
    _tmux("set-option", "-t", name, "-a", "terminal-features", ",xterm-256color:extkeys")
    return our_sid


def pty_attach(our_sid: str, cols: int = 80, rows: int = 24) -> tuple[int, subprocess.Popen]:
    """Attach to a tmux session via a real pty pair.

    Returns ``(master_fd, process)``. The caller owns the fd and must close it
    when done; closing the master fd causes ``tmux attach`` to exit cleanly
    (the session itself keeps running).

    This is the same architecture used by ttyd/wetty/gotty: a pty master fd
    provides bidirectional bytes I/O with the terminal — no pipe-pane, no
    send-keys, no FIFO.
    """
    if not TMUX_BIN:
        raise RuntimeError("tmux not installed")
    name = _session_name(our_sid)
    master_fd, slave_fd = pty_mod.openpty()
    # Set initial pty size before attach so tmux sees the right geometry
    winsize = struct.pack("HHHH", rows, cols, 0, 0)
    fcntl.ioctl(master_fd, termios.TIOCSWINSZ, winsize)
    env = os.environ.copy()
    env["TERM"] = "xterm-256color"
    proc = subprocess.Popen(
        [TMUX_BIN, "-L", SOCKET_NAME, "attach-session", "-t", name],
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        env=env,
        preexec_fn=os.setsid,
    )
    os.close(slave_fd)
    # Non-blocking so asyncio can poll the fd
    flags = fcntl.fcntl(master_fd, fcntl.F_GETFL)
    fcntl.fcntl(master_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)
    return master_fd, proc


def pty_resize(master_fd: int, cols: int, rows: int) -> None:
    """Update the pty dimensions. tmux sees the SIGWINCH and resizes."""
    winsize = struct.pack("HHHH", rows, cols, 0, 0)
    fcntl.ioctl(master_fd, termios.TIOCSWINSZ, winsize)


def _start_pipe(session_name: str, our_sid: str) -> None:
    """Ensure a FIFO exists and pipe-pane writes to it.

    Each call unconditionally replaces the pipe. `pipe-pane` without
    `-o` kills any prior `cat` process and starts a fresh one writing
    to our newly-mkfifo'd path. Using `-o` (toggle) is wrong here:
    on WS reconnect the prior cat still has an fd to the now-unlinked
    fifo and keeps writing into a dangling inode, while the new fifo
    has no writer — xterm's grid silently drifts away from tmux's as
    emit bytes are lost to the void.
    """
    fifo = _fifo_path(our_sid)
    try:
        if fifo.exists():
            fifo.unlink()
        os.mkfifo(fifo, mode=0o600)
    except FileExistsError:
        pass
    _tmux(
        "pipe-pane",
        "-t", session_name,
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


def send_shift_enter(our_sid: str) -> None:
    """Send Shift+Enter (CSI u: ESC[13;2u) as raw hex bytes into the pane.

    Uses send-keys -H to bypass tmux's input parser entirely, injecting
    the bytes directly into the pane's stdin.
    """
    if not tmux_available():
        return
    # \x1b [ 1 3 ; 2 u
    _tmux("send-keys", "-H", "-t", _session_name(our_sid),
          "1b", "5b", "31", "33", "3b", "32", "75")


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
    """Return the currently visible pane content as escape-preserving bytes.

    Intentionally does NOT include scrollback — capturing scrollback after a
    resize returns stacked renders (Claude draws its TUI before SIGWINCH, then
    again after), which paints as duplicated content in xterm. The live FIFO
    stream picks up from the current frame forward.

    `lines` is retained for API compatibility but currently unused; pass it if
    you later want scrollback (use `-S -{lines}`).
    """
    if not tmux_available():
        return b""
    res = _tmux(
        "capture-pane",
        "-p",       # print to stdout
        "-e",       # include escape sequences
        "-t", _session_name(our_sid),
    )
    if res.returncode != 0:
        return b""
    return res.stdout.encode("utf-8", errors="replace")


def cursor_position(our_sid: str) -> tuple[int, int] | None:
    """Return (x, y) cursor position in tmux's virtual grid, or None.

    `capture_pane` dumps grid cells only — no CUP — so after writing a
    snapshot into xterm the cursor lands wherever the last byte fell
    (typically the bottom of the pane after trailing \\n's). Callers
    should emit `\\e[y+1;x+1H` after the snapshot to reposition.
    """
    if not tmux_available():
        return None
    res = _tmux(
        "display-message",
        "-p",
        "-t", _session_name(our_sid),
        "#{cursor_x}|#{cursor_y}",
    )
    if res.returncode != 0:
        return None
    parts = res.stdout.strip().split("|")
    if len(parts) != 2:
        return None
    try:
        return int(parts[0]), int(parts[1])
    except ValueError:
        return None


def close_pipe(our_sid: str) -> None:
    """Close any active pipe-pane on the session.

    Calling `pipe-pane` with no command shuts down the existing cat
    process and lets tmux drain its internal pipe buffer to /dev/null.
    Any bytes Claude emitted since the last snapshot are lost, but
    we're about to capture a fresh snapshot anyway — so this is
    exactly what we want. A subsequent `_start_pipe` call then creates
    a genuinely fresh pipe with no stale byte prefix.
    """
    if not tmux_available():
        return
    _tmux("pipe-pane", "-t", _session_name(our_sid))


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


def send_enter(our_sid: str) -> None:
    """Send a discrete Enter key event, distinct from a raw \\r byte.

    Claude's TUI uses paste-vs-typing detection: a \\r arriving in the same
    read buffer as the prompt text is treated as a newline inside a paste,
    not as submit. Using tmux's named key and a small delay ensures the
    Enter lands as its own keystroke event after the text has been absorbed.
    """
    if not tmux_available():
        return
    subprocess.run(
        [TMUX_BIN, "-L", SOCKET_NAME, "send-keys", "-t", _session_name(our_sid), "Enter"],
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
