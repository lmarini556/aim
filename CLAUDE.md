# AIM — Agent Instances Manager

Native macOS Tauri v2 app for managing Claude Code instances. Rust backend (embedded axum HTTP/WS + Tauri shell) + static HTML/JS frontend.

## Layout

```
claude-instances-ui/
├── src/              Rust crate (binary: aim, library: aim_lib)
│   ├── src/
│   │   ├── domain/   Pure types (instance, transcript, error)
│   │   ├── infra/    IO (paths, tmux, pty)
│   │   ├── services/ Business logic (instances, transcript, summarizer, config, hooks)
│   │   ├── http/     Axum router, DTOs, handlers, auth
│   │   └── native/   Tauri tray, poller, notifications, sound, webview menu
│   ├── tauri.conf.json
│   └── resources/    Bundled tmux + hook_writer.py
├── static/           Frontend bundle (served by axum as frontendDist)
│   ├── app.js, index.html, sprite.js, style.css
│   ├── lib/          Pure ES modules imported by app.js
│   └── vendor/       xterm.js family
└── tests/frontend/   Vitest suite (outside static/ so Tauri doesn't bundle node_modules)
    └── *.test.js, package.json, vitest.config.js
```

## Running

```bash
# backend + native shell (single binary starts axum on 7878 AND opens Tauri window)
cd src && cargo run --bin aim

# DO NOT use `cargo tauri dev` — it polls devUrl before launching the binary,
# deadlocking because the binary IS the server.
```

## Testing

### Rust (`cd src`)

```bash
cargo test --lib                      # 658 unit tests
cargo llvm-cov --lib --summary-only   # coverage report
```

### Frontend (`cd tests/frontend`)

```bash
npm test              # vitest run
npm run coverage      # v8 coverage on ../../static/lib/*.js
```

## Coverage targets — maintain these

Non-native code must stay at **≥97%** regions/funcs/lines. Native/platform-gated files (`native/webview_menu.rs`, `infra/tmux/pty.rs`, `native/tray.rs`, `native/notifications.rs`, `native/poller.rs`, `http/handlers/terminal.rs` ws body) are exempt — they require a live Tauri event loop or real terminal.

**Current baseline** (update when it changes):

| Surface | Regions | Funcs | Lines | Tests |
|---|---|---|---|---|
| Rust total | 97.06% | 97.06% | 96.90% | 658 |
| Frontend `lib/pure.js` | 100% | 100% | 100% | 69 |

**Rules:**
1. New non-native Rust code: add tests in the same PR. Do not merge with uncovered regions.
2. Before declaring any task complete, run `cargo llvm-cov --lib --summary-only` and `npm run coverage` in `static/`. If totals regressed, either add tests or justify in the task.
3. When refactoring existing modules, re-check coverage — LLVM region counts shift with code layout.
4. If you intentionally lower coverage (e.g., deleting tested code), update the baseline table in this file.
5. Never suppress coverage with `#[cfg(not(tarpaulin_include))]` or similar to hit a number. Exclusions are whitelisted only for platform-gated native modules listed above.

## Architecture notes

- **Embedded axum server, not Tauri IPC.** Frontend uses `fetch()` + `WebSocket()` against `127.0.0.1:7878`. Keeps xterm.js ↔ pty bridge simple.
- **Token auth.** Generated on first run, stored at `~/.claude-instances-ui/token`. Frontend sends as `?t=…` (becomes Bearer header).
- **Hooks.** Claude Code writes state to `~/.claude-instances-ui/state/{session_id}.json` via `hook_writer.py` (installed by `services::hooks::install`).
- **PTY ownership.** When spawning tmux attach, each of stdin/stdout/stderr must be its own `OwnedFd` (use `try_clone()`). Triple-assigning the same raw fd trips Rust's IO safety check and aborts.

## Release

Brew cask lives at `../homebrew-tap/Casks/aim.rb`. After tagging a new version in this repo and pushing the release artifact, bump the `version` and `sha256` in the cask.
