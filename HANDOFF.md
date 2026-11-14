# Riff Rust — handoff for the next AI

Read this first. Then `PORTING.md` for the feature-by-feature porting status.

## What this is

A from-scratch rebuild of [Riff](https://github.com/rootscripts/riff) (Electron + vanilla JS, MIT, by rootscripts) in **Rust + Tauri v2**. Windows-first, NSIS installer, no bundled Chromium, no Node.

- The **entire UI is the upstream renderer** (`src/renderer`), served by WebView2 — that's how "exact the same UI" is preserved. Do not port it to native unless the owner explicitly asks for a full native rewrite.
- The **backend is Rust**: an embedded axum HTTP server on `127.0.0.1:<random port>` (see `src-tauri/src/server.rs`) plus Tauri commands (`src-tauri/src/commands.rs`), exposed to the UI through `src/renderer/electron-shim.js`, which masquerades as Electron's `window.electronAPI`.
- Upstream Electron code (`src/main`, `src/server`) was removed; `git log` has the history if you ever need the old logic. The upstream repo is public.

## Repo map

| Path | What it is |
|---|---|
| `src/renderer/` | The UI. `app.js` (~44k lines) is upstream; `electron-shim.js` is ours. |
| `src-tauri/src/` | Rust: `main.rs` (window/tray/boot), `server.rs` (embedded API), `commands.rs` (Tauri IPC), `library.rs` (import/tags via lofty), `state.rs`, `util.rs` |
| `scripts/assemble.ps1` | Flattens renderer + assets + vendor into `web/`, rewrites `../../assets/` refs, injects `electron-shim.js` into the HTML. **Edit sources in `src/renderer`, never `web/` (build artifact).** |
| `PORTING.md` | Authoritative feature status. Keep it updated. |
| `release/` | Installer artifacts (gitignored). |

## Build & release

```bat
npm install
npm run assemble
npm run build    # -> src-tauri/target/release/bundle/nsis/
```

Windows needs: Node 18+, Rust (MSVC), WebView2 (already on Win10/11). First run auto-downloads yt-dlp; ffmpeg via `winget install Gyan.FFmpeg`.

Release process used for v0.1.x: bump `version` in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`; commit; tag; `gh release create` with the NSIS `.exe`. Repo: `ziyyhhk/riff-rust` (public, prereleases).

## Hard-won gotchas

1. The axum listener must be **nonblocking before `TcpListener::from_std`** or all requests hang (fixed).
2. The shim needs `core:event:default` + `dialog:default` in `src-tauri/capabilities/default.json` or `__TAURI__.event.listen` and the file dialogs fail with ACL errors (fixed).
3. Playback CORS: the webview is cross-origin to `127.0.0.1:<port>`; all `/api/*` responses need `Access-Control-Allow-Origin: *` (tower-http CorsLayer, done). Removing it breaks streaming and the X-Riff-Token preflight.
4. `Cargo.lock` is committed; `cargo clean` costs ~10–15 min full rebuild, so think before cleaning the cache.
5. The tutorial marker is `%LOCALAPPDATA%\Riff Rust\.rifftutorial` (must contain the app version) — delete it to re-test the first-run wizard.
6. `src-tauri/gen/` is build-generated (schemas) — never commit it (gitignored).
7. Smoke test: launch the release exe, read `%LOCALAPPDATA%\Riff Rust\logs\riff-rust.log` ("serve thread started"…"axum serve starting"), and curl `127.0.0.1:<port>` endpoints. The renderer will `/api/youtube-account-info` at boot (stub = normal).

## Done (see PORTING.md)

First-run onboarding wizard (upstream-identical), launcher Start-menu shortcut (Windows), local library (import/tags/covers/range streaming), YouTube/SoundCloud search + streaming + downloads via yt-dlp, LRCLIB + custom lyrics, public Spotify playlist import (anonymous embed-token pathfinder), YouTube/SoundCloud playlist import, tray + close-to-tray, cache/data-dir management, settings persistence (`storage.json`).

## Not done (in priority order if continuing parity)

1. Google/YouTube sign-in & import (OAuth PKCE + account file) — upstream: `google-oauth.js`, `youtube-data.js`
2. Spotify account integration (auth, library) — import works, account features don't
3. Apple Music / Deezer / Yandex / TikTok / VK import
4. Recommendations/discover (`/api/vibe`, `/api/artist`, trending/hot/charts)
5. i18n batch translations (Korean UI strings for welcome/app), karaoke word-level lyrics
6. Auto-updater (no feed), plugin execution sandbox, Discord RPC socket client, mini player, media keys

## Honesty rules

- Don't claim parity. "Same UI" is true; "same features" is not yet.
- Memory claim is unverified — nobody has measured it honestly yet. Do not overstate.
- Keep `PORTING.md` current; the owner reads it.
