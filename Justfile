# claude-instances-ui dev helpers.
# Run `just` with no args to see the recipe list.

# Default: show available recipes.
default:
    @just --list --unsorted

# Kill the server, sync the Hammerspoon lua + reload, relaunch the server,
# and as soon as it's listening open the authenticated dashboard URL in
# your default browser. This is the one you want bound to a hotkey.
refresh: stop reload-hammerspoon
    #!/usr/bin/env bash
    port="${CIU_PORT:-7878}"
    (
      for i in $(seq 1 120); do
        curl -sS -o /dev/null --connect-timeout 1 "http://127.0.0.1:$port/" 2>/dev/null && break
        sleep 0.15
      done
      token=$(cat ~/.claude-instances-ui/token 2>/dev/null || true)
      [ -n "$token" ] && open "http://127.0.0.1:$port/?t=$token"
    ) &
    exec uv run server.py

# Start the server in the foreground (blocks; Ctrl-C to stop).
up:
    uv run server.py

# Kill any running server process. Non-zero exit codes from pkill (no match)
# are ignored so this is always safe to chain.
stop:
    -@pkill -f "server.py" 2>/dev/null || true
    @sleep 0.3

# Copy the working-copy Hammerspoon lua into ~/.hammerspoon/ and reload the
# Hammerspoon config. Requires the hammerspoon:// URL scheme to be enabled
# (it is by default).
reload-hammerspoon:
    cp menubar/claude_instances.lua ~/.hammerspoon/
    open -g "hammerspoon://reload"

# Fire a test notification from Hammerspoon to verify the notify pipeline.
# Watch both the screen (themed banner) and Notification Center.
test-notify:
    osascript -e 'tell application "Hammerspoon" to execute lua code "require(\"claude_instances\").testNotify()"'

# Install Claude Code hooks. Writes ~/.claude-instances-ui/bin/hook_writer.py
# and registers it in ~/.claude/settings.json. Idempotent.
install-hooks:
    python3 install_hooks.py

# Uninstall Claude Code hooks (removes the settings.json entries).
uninstall-hooks:
    python3 install_hooks.py uninstall

# Tail the Hammerspoon log (handy while iterating on the lua).
hs-log:
    tail -f ~/.hammerspoon/console.log 2>/dev/null || echo "no console.log — open Hammerspoon console with ⌘⌃D"

# Open the dashboard in the default browser (uses the saved token file).
open:
    @token=$(cat ~/.claude-instances-ui/token 2>/dev/null); \
    if [ -z "$token" ]; then echo "no token yet — run 'just up' once"; exit 1; fi; \
    open "http://127.0.0.1:7878/?t=$token"
