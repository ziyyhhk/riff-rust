# Riff Rust — porting status

Riff Rust is a from-scratch Rust/Tauri rebuild of [Riff](https://github.com/rootscripts/riff)
(Electron + vanilla JS, MIT). The UI is the original `src/renderer` (HTML/CSS/JS, ~same code),
served by the OS webview (WebView2 on Windows) instead of a bundled Chromium.
The Electron main process and Node server were replaced with Rust.

## Status legend

- ✅ Ported and working
- 🟡 Behaviour differs or is partial
- ❌ Not yet ported (returns an honest error/empty state; UI must not crash)

## Architecture

| Piece | Riff (upstream) | Riff Rust |
|---|---|---|
| UI runtime | Electron (Chromium) | Tauri v2 → WebView2 / WKWebView / WebKitGTK |
| Backend | `src/server/server.js` (Node http) | embedded Rust server (axum) on `127.0.0.1:<random port>` |
| Main process | `src/main/main.js` (Electron IPC) | Tauri commands (`window.electronAPI` shim → `invoke()`) |
| Audio DSP / EQ / visualizers | Web Audio API + WebGL (renderer) | **unchanged** — runs in the webview |
| Data dir | `%LOCALAPPDATA%\Riff` | `%LOCALAPPDATA%\Riff Rust` |

## What works today (v0.1)

- ✅ Same UI (styles.css, index.html, app.js, themes, assets)
- ✅ First-run onboarding wizard — identical flow to upstream (language → look → make it yours → launcher → connect), shown only on first launch (versioned `.rifftutorial` marker)
- ✅ "Start Menu shortcut" launcher integration on Windows (creates a real `.lnk`)
- ✅ Frameless window, tray icon (Show / Quit), close-to-tray
- ✅ Local library: import audio files (file dialog → copied to library dir),
  tags read with `lofty` (title/artist/album/duration/cover), list/remove/update,
  range-request streaming with seeking (`/api/local-track`, `/api/library-file`)
- ✅ YouTube / SoundCloud **search** via yt-dlp (`ytsearch`/`scsearch`)
- ✅ Stream resolution via yt-dlp (`/api/stream-info`) with local-cache short circuit
- ✅ Downloads via yt-dlp + ffmpeg (`/api/download`, mp3/flac/opus/wav)
- ✅ Tools status / auto-download of yt-dlp (`/api/tools/status`, `/api/tools/install`)
- ✅ Lyrics via LRCLIB + custom `.lrc` lyrics (`/api/lyrics`)
- ✅ Spotify playlist import (anonymous embed-token pathfinder flow, same as upstream; `/api/spotify-import` + `/api/playlist-import` for YouTube/SoundCloud playlists via yt-dlp)
- ✅ Offline library listing/deletion (`/api/offline-library`, `/api/offline-delete`, `/api/clear-offline-library`)
- ✅ Image proxy + audio proxy with Range forwarding (`/api/image-proxy`, `/api/audio-proxy`)
- ✅ Local thumbnail serving, cache size/clear, data-dir & download-dir management,
  storage persistence (`storage.json`), playlists export (`playlists.json`)
- ✅ Themes/plugins import & enable state (`.css` / `.js` drop-ins)
- ✅ Diagnostics, logs (`logs/riff-rust.log`), GPU compositing flag (webview2)
- 🟡 Discord Rich Presence: toggle plumbing exists, RPC itself not wired (needs a Discord socket client)
- 🟡 Plugin execution: listable/toggleable, but sandboxed runner not built
- 🟡 Proxy config: stored and applied to yt-dlp + server-side fetches; no per-route test UI wiring

## Not yet ported ❌ (returns `{ success: false, "error": "not yet ported…" }`)

- Google / YouTube account sign-in & library import (OAuth PKCE flow, account encryption)
- Spotify integration (auth, personal library) — but **public playlist import works**
- Apple Music, Deezer, Yandex, TikTok, VK playlist import
- Last.fm (scrobbling, token flow)
- Discovery/recommendations (`/api/vibe`, `/api/artist`, `/api/trending`, `/api/hot`, `/api/top-chart`)
- i18n/translation batch API, karaoke word-level lyrics (`/api/lyrics-words`)
- Audio editor/trim/region-embed pipeline (editor UI loads, but backend trim export is unported)
- Auto-updater (no release feed configured for Riff Rust yet)
- Mini-player window, global media keys / global shortcuts registration
- Playlist context menus (native); launcher integration outside Windows
- Welcome/onboarding flow (window loads straight to the app)
- Export/import of the encrypted account file (`RFAC` format)

## Why it should use less RAM/CPU

Upstream ships a whole Chromium per window (~180–600 MB). Riff Rust ships only the native
webview plus a small Rust backend; the heavy parts upstream already did in the renderer
(Web Audio EQ/DSP, WebGL visualizers) still run there, but there is no extra browser process,
no V8/Node backend, and the audio DSP path is identical.

## Known limitations of v0.1

- Windows-only installer; the code builds on macOS/Linux but tray/menu polish is Windows-first.
- `yt-dlp` + `ffmpeg` are not bundled (size); yt-dlp self-installs on first run from the
  official GitHub release, ffmpeg must come from winget/choco/scoop or `bin/`.
- Streaming YouTube depends on yt-dlp keeping pace with YouTube changes, same as upstream.
- Local library IDs match upstream (`local_<sha1(name+size)>`), so a data migration of local
  files is straightforward, but the Riff Rust data dir is separate from Riff's.

## Build

```bat
npm install
npm run assemble   # src/renderer + assets + vendor -> web/
npm run build      # cargo tauri build -> NSIS installer
```

`scripts/assemble.ps1` flattens the web assets and rewrites `../../assets/` references;
edits to the renderer must happen in `src/renderer`, then re-run assemble.

## Data location

`%LOCALAPPDATA%\Riff Rust\` (library/, tracks/, local-covers/, logs/, storage.json, config.json, library.json)
