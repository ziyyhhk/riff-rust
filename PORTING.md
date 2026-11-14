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

## What works today (v0.1.2)

- ✅ Same UI (styles.css, index.html, app.js, themes, assets)
- ✅ First-run onboarding wizard — identical flow to upstream (language → look → make it yours → launcher → connect), shown only when `.rifftutorial` is absent (same existence-only marker as upstream)
- ✅ "Start Menu shortcut" launcher integration on Windows (creates a real `.lnk`)
- ✅ Frameless window, tray icon (Show / Quit), close-to-tray
- ✅ Local library: import audio files (file dialog → copied to library dir),
  tags read with `lofty` (title/artist/album/duration/cover), list/remove/update,
  HTTP Range streaming with seeking (`/api/local-track`, `/api/library-file`).
  Ranges are streamed from disk in 64 KiB chunks; the whole track is never buffered in RAM.
- ✅ YouTube Music **search** through the inner API (`src-tauri/src/ytm.rs`) with a
  5-minute cache — no Python process per search. Falls back to yt-dlp if the API changes.
- ✅ SoundCloud **search** via yt-dlp (`scsearch`)
- ✅ Stream resolution via yt-dlp (`/api/stream-info`) with a 10-minute resolved-URL cache
  and a downloaded-file short circuit; yt-dlp runs on a blocking worker and is killed on timeout
- ✅ Downloads via yt-dlp + ffmpeg (`/api/download`, mp3/flac/opus/wav)
- ✅ Tools auto-install (`src-tauri/src/tools.rs`): upstream's `ensureTools` ported to Rust —
  yt-dlp and ffmpeg are fetched from the same release URLs on boot or via `/api/tools/install`,
  SHA-256 verified against the published SUMS, extracted (PowerShell → Expand-Archive → tar.exe),
  with download progress polled by the required-tools modal (`/api/tools/status`). winget fallback
  (Gyan.FFmpeg / yt-dlp.yt-dlp) when the direct download fails. No more endless "downloading tools".
- ✅ Lyrics via LRCLIB + custom `.lrc` lyrics (`/api/lyrics`)
- ✅ Spotify playlist import (anonymous embed-token pathfinder flow, same as upstream; `/api/spotify-import` + `/api/playlist-import` for YouTube/SoundCloud playlists via yt-dlp)
- ✅ Offline library listing/deletion (`/api/offline-library`, `/api/offline-delete`, `/api/clear-offline-library`),
  with exact-id matching so deleting one track cannot remove another with the same prefix
- ✅ Image proxy + audio proxy with Range forwarding (`/api/image-proxy`, `/api/audio-proxy`)
- ✅ Local thumbnail serving, cache size/clear, data-dir & download-dir management,
  storage persistence (`storage.json`), playlists export (`playlists.json`)
- ✅ Themes/plugins import & enable state (`.css` / `.js` drop-ins)
- ✅ Diagnostics, logs (`logs/riff-rust.log`) with 4 MB rotation (3 old files kept)
- ✅ Media keys (play/pause, next, previous, stop) and the app's global shortcuts
  (`Ctrl+Shift+Space/Left/Right/Up/Down`) through `tauri-plugin-global-shortcut`.
  Media keys are only grabbed after the renderer reports its audio graph is ready, matching upstream.
- ✅ Mini-player: toggles to a 380×160 always-on-top window, remembers geometry, restores on exit.
  Mini lyrics expand/collapse the window and stay on-screen.
- ✅ Window startup/geometry matches upstream Electron: 82%×86% of the primary **work area**
  (taskbar excluded) at 9%/7% offsets, saved `window-state.json` (`bounds`/`isMaximized`/
  `isFullScreen`) restored onto the matching display, hidden until the page loads, geometry
  persisted on resize/move/maximize/fullscreen/close (400 ms throttled).
- ✅ Window visibility events (`window-visibility`) on minimize/restore/hide/show, so the renderer
  can pause visualizers/background video while hidden.
- ✅ Shared pooled HTTP client (connection reuse, gzip) and bounded TTL caches for search/stream URLs
- ✅ "Riff Library" hybrid search (`platform=rifflibrary`): single ranked list combining YouTube Music + SoundCloud interleaved 2:1, like upstream's unified pool. `/api/stream-info` returns `streamUrl`/`proxyUrl`/`rawStreamUrl` in upstream's shape; audio plays through the loopback `/api/audio-proxy` (edition that makes the Web Audio chain work), and resolved streams are cached to `tracks/<id>` in the background so repeat plays are local.
- ✅ Discovery/recommendations (`src-tauri/src/discovery.rs`): country charts (`/api/top-chart`,
  YouTube Charts playlist browse via YTM inner API + iTunes RSS + Last.fm fallback), `/api/vibe`
  (seeded by YTM RDAMVM mixes + Deezer related artists, 20-minute cache), `/api/trending`
  (Deezer scene charts + YTM search, 30-minute cache), `/api/hot` (global Deezer chart resolved
  through YTM), `/api/artist` (Deezer top tracks/albums/fans + Last.fm bio & monthly listeners).
  The home page now fills like upstream: For You / Trending / Top chart.
- 🟡 Discord Rich Presence: toggle plumbing exists, RPC itself not wired (needs a Discord socket client)
- 🟡 Plugin execution: listable/toggleable, but sandboxed runner not built
- 🟡 Proxy config: stored and applied to yt-dlp + server-side fetches; no per-route test UI wiring

## Not yet ported ❌ (returns `{ success: false, "error": "not yet ported" }`)

- Google / YouTube account sign-in & library import (OAuth PKCE flow, account encryption)
- Spotify integration (auth, personal library) — but **public playlist import works**
- Apple Music, Deezer, Yandex, TikTok, VK playlist import
- Last.fm (scrobbling, token flow; only used anonymously for artist bio/listeners)
- i18n/translation batch API, karaoke word-level lyrics (`/api/lyrics-words`)
- Audio editor/trim/region-embed pipeline (editor UI loads, but backend trim export is unported)
- Auto-updater (no release feed configured for Riff Rust yet)
- Playlist context menus (native); launcher integration outside Windows
- Export/import of the encrypted account file (`RFAC` format)

## Why it should use less RAM/CPU

Upstream ships a whole Chromium per window (~180–600 MB). Riff Rust ships only the native
webview plus a small Rust backend; the heavy parts upstream already did in the renderer
(Web Audio EQ/DSP, WebGL visualizers) still run there, but there is no extra browser process,
no V8/Node backend, and the audio DSP path is identical.

Backend-specific wins over the first Rust build (v0.1.1):

- Local playback streams ranges from disk instead of reading a whole track into memory per
  request (a 60 MB FLAC no longer costs 60 MB RAM on every seek).
- One pooled `reqwest::Client` instead of a new client/TLS handshake per request.
- Search runs through the Rust YTM inner API by default (~300–1500 ms, bound by YouTube's
  servers) instead of spawning `yt-dlp`/Python per query; resolved stream URLs are cached for
  10 minutes.
- Embedded API client timeout is 8 s (connect 6 s): a stalling YouTube degrades in seconds
  instead of hanging for 30.
- yt-dlp resolution uses `--socket-timeout 8 --retries 1`: bad networks fail fast; the
  localhost audio proxy remains a true streaming proxy (bytes forwarded, nothing buffered).
- yt-dlp children are killed on timeout instead of being left running.
- Logs rotate at 4 MB; `cacheLimitMB` in `config.json` optionally trims `tracks/` at startup.
- Minimized windows now tell the renderer, letting it stop visualizer/background work.

## Performance tooling

```bat
npm run measure                :: RAM/CPU for the whole process tree (riff-rust + WebView2)
npm run measure -- -Seconds 30
npm run measure -- -ExePath .\src-tauri\target\release\riff-rust.exe
npm run clean                  :: remove web/ + release/ + logs + target/debug/
npm run clean:all              :: also remove node_modules/ and src-tauri/target/
```

`measure.ps1` sums every child process (WebView2 renderer/GPU, yt-dlp/ffmpeg) and reports root
vs tree working set, peak private bytes, and CPU. Measuring only `riff-rust.exe` understates RAM
by an order of magnitude.

No comparative numbers are claimed yet; measure the same playback scenario in upstream Riff
and Riff Rust before publishing a RAM/CPU figure.

## Known limitations of v0.1

- Windows-only installer; the code builds on macOS/Linux but tray/menu and media-key behaviour
  is only smoke-tested on Windows.
- `yt-dlp` + `ffmpeg` are not bundled (size); both auto-install on first run from the official
  GitHub releases (SHA-256 verified), with winget fallback. ffmpeg is ~70 MB, so the first launch
  depends on network speed.
- Streaming YouTube depends on yt-dlp keeping pace with YouTube changes, same as upstream.
- The YouTube Music inner-API search is unofficial and can break; the yt-dlp fallback keeps
  search working, just slower.
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
