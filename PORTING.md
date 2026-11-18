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

## What works today (v0.1.19)

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
- ✅ SoundCloud **search and stream resolution** through the direct SoundCloud API
  (`src-tauri/src/soundcloud.rs`): credentials scraped from the JS bundles on the front
  page (cached 12 h), `/search/tracks` for search (~1 s), `/resolve` for progressive
  MP3 stream URLs. Falls back to yt-dlp if SoundCloud changes anything.
- ✅ Stream resolution via yt-dlp (`/api/stream-info`) with a 10-minute resolved-URL cache
  and a downloaded-file short circuit; yt-dlp runs on a blocking worker and is killed on timeout
- ✅ Downloads via yt-dlp + ffmpeg (`/api/download`, mp3/flac/opus/wav)
- ✅ Tools auto-install (`src-tauri/src/tools.rs`): upstream's `ensureTools` ported to Rust —
  yt-dlp and ffmpeg are fetched from the same release URLs on boot or via `/api/tools/install`,
  SHA-256 verified against the published SUMS, extracted (PowerShell → Expand-Archive → tar.exe),
  with download progress polled by the required-tools modal (`/api/tools/status`). winget fallback
  (Gyan.FFmpeg / yt-dlp.yt-dlp) when the direct download fails. No more endless "downloading tools".
- ✅ Lyrics via LRCLIB + custom `.lrc` lyrics (`/api/lyrics`). Now also: cached 24 h, `[offset:]`
  LRC tags honoured (upstream drops them — the classic "lyrics run behind the vocals" bug),
  and `/api/lyrics-words` wired to NetEase enhanced-LRC for word-level karaoke.
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
- ✅ Direct inner-API audio resolution (v0.1.7 src-tauri/src/ytm_player.rs): /api/stream-info tries YouTube /youtubei/v1/player first (IOS + ANDROID_MUSIC, visitorData hot-swap) before falling back to yt-dlp; cold track resolves run 0.3-1.5s vs ~8s before. Also "Riff Library" hybrid search (platform=rifflibrary): single ranked list combining YouTube Music + SoundCloud interleaved 2:1; /api/stream-info returns streamUrl/proxyUrl upstream shape, audio plays through loopback /api/audio-proxy.
- ✅ MP3-first caching (v0.1.8): the background cache now downloads or **re-encodes** every stream to `tracks/<id>.mp3` (mp3 CDN links are stored byte-for-byte, everything else goes through ffmpeg). Unplayable cache files (.m4a/.webm) already on disk are transcoded to mp3 on the next play and then deleted. MP3/WAV/FLAC are probed before anything else everywhere, so repeat plays are local-file instant and survive machines without AAC/Opus decoders (this Windows build cannot decode YouTube's streams at all; WebView2 reports code=4).
- ✅ Renderer decode-failure recovery (v0.1.8): the renderer's automatic second retry carries `forceYtDlp=1`, which Riff Rust now honours: /api/stream-info resolves with the inner API, transcodes to a local MP3 in a blocking task, and returns a `/api/local-track` URL. The player gets a file it can decode on the first retry, with no user action. Falls back to the old yt-dlp-then-proxy flow only if ffmpeg or the resolver fails.
- ✅ Discovery/recommendations (`src-tauri/src/discovery.rs`): country charts (`/api/top-chart`,
  YouTube Charts playlist browse via YTM inner API + iTunes RSS + Last.fm fallback), `/api/vibe`
  (seeded by YTM RDAMVM mixes + Deezer related artists, 20-minute cache), `/api/trending`
  (Deezer scene charts + YTM search, 30-minute cache), `/api/hot` (global Deezer chart resolved
  through YTM), `/api/artist` (Deezer top tracks/albums/fans + Last.fm bio & monthly listeners).
  The home page now fills like upstream: For You / Trending / Top chart.
- ✅ Discord Rich Presence: IPC named pipe discord-ipc-{0..9}, READY handshake, SET_ACTIVITY with track title/artist/duration/thumbnail/Listen button, PING-PONG, 15s reconnect.
- 🟢 Plugins **execute renderer-side** (v0.1.19), like upstream: `list_plugins` ships the
  source (≤256 KB) plus JSDoc-ish header metadata (`@name`/`@author`/`@version`/`@description`)
  and the renderer builds them with `new Function('Riff', 'plugin', code)`. There is no
  Rust-side JS runtime; plugin trust model = user-installed code, same as upstream.

## Fixed in v0.1.19

- **Custom data directory actually works now.** The override was written to `config.json["dataDir"]` but nothing ever read it. v0.1.19 adds a fixed-location bootstrap pointer (`%LOCALAPPDATA%\Riff Rust Boot\data-dir.json`, outside the data dir) which `data_dir()` reads (memoized per process). `set_data_dir` writes the pointer **after** the file copy succeeds (and now copies `config.json` too, into a 13-entry list), returns `requiresRestart:true`, and `get_data_dir` reports `isCustom` truthfully. A moved/missing pointer target falls back to the platform default with one WARN, never a panic, never deletes data. Take effect: next launch.
- `/api/state`, `/api/save-track-source`, `/api/save-default-settings` no longer return
  misleading success/error responses: karaoke `/api/state` now stores the now-playing payload;
  track-source resolution persists to `<dataDir>/track-sources.json` (cap 5000 entries, oldest
  evicted); settings backup writes `<dataDir>/defaults-saved.json` atomically (512 KB / 2000 key
  cap). `/api/save-default-settings` is no longer "read-only error" theater.
- `unwrap()` panics removed from `/api/local-thumbnail`, `/api/image-proxy`, `/api/audio-proxy`
  (and a poisoned-lock `.unwrap()` in `/api/download`) — failures return HTTP 500 JSON instead.
- `/api/trending`, `/api/hot`, `/api/top-chart` fail closed (`success:false`, `tracks:[]`) when
  every source fails, instead of caching/serving empty charts as success; empty cached charts
  are no longer served. Renderer falls back to a cached chart or hides the section.
- `offline_library` shows real title/artist from ID3 tags for cached MP3s (10 min tag cache)
  instead of the bare video id.
- `best_search_hit` no longer borrows `hits[0]`; empty-input is a first-class `None`.
- Last.fm API key unified onto one constant (was two different embedded keys).
- Renderer: list/sidebar thumbnails request `w128` CDN copies instead of `w544`
  (≈18x fewer decoded pixels per row), rows skip layout/paint off-screen
  (`content-visibility: auto` + `contain-intrinsic-size`), `decoding="async"` added,
  dead code removed.
- WebView2: dropped the Android/ChromeOS-only `--enable-low-end-device-mode` placebo flag;
  added `--disable-background-networking --disable-component-update --disable-domain-reliability
  --no-pings`; the WebView2 memory usage target stays `LOW` permanently (measured: `NORMAL`
  while visible filled the webview working sets to ~590 MB tree on this machine with no
  frame-pacing benefit; 0.1.17/0.1.18 shipped always-`LOW` at ~99–115 MB and held 60 fps).
- Full honesty note: **40 MB was a fantasy target.** The floor is the WebView2 process
  group (browser + renderer + GPU + audio service); 0.1.17 measured ~96 MB and 0.1.18 ~99–115 MB
  whole-tree on this machine. v0.1.19 reduces what is legitimately reducible (decode work,
  paint, background noise); the release notes carry the measured post-build figure instead
  of a promised one.
- Unused `assets/Tone.js` (~345 KB) removed.

## Partial / still 🟡 (v0.1.19)

- 🟡 Proxy config: stored and applied to yt-dlp + server-side fetches; no per-route test UI wiring
- i18n/translation batch API is implemented (`/api/i18n-batch`: DeepL → Google gtx → MyMemory);
  `/api/translate` remains an honest stub.

## Not yet ported ❌ (returns `{ success: false, "error": "not yet ported" }`)

- Google / YouTube account sign-in & library import (OAuth PKCE flow, account encryption)
- Spotify integration (auth, personal library) — but **public playlist import works**
- Apple Music, Deezer, Yandex, TikTok, VK playlist import
- Last.fm (scrobbling, token flow; only used anonymously for artist bio/listeners)
- Karaoke word-level lyrics ARE ported (`/api/lyrics-words`, NetEase yrc); `/api/translate` is an honest stub while `/api/i18n-batch` works (DeepL → Google gtx → MyMemory).
- Audio editor/trim/region-embed pipeline (editor UI loads, but backend trim export is unported)
- Self-updater exists but releases are manual: CI builds the NSIS installer as an artifact and
  never publishes; the in-app updater checks `ziyyhhk/riff-rust`'s latest release, so it works,
  but every update ships via a hand-made release.
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
- Streams are now cached as MP3 (re-encoded on the way in), so repeat plays never touch the
  network or the decode-fragile path; this machine decodes MP3/WAV/FLAC only.
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
- This machine's WebView2 build cannot decode AAC (m4a/mp4) or WebM Opus at all, while
  Electron-shipped Chromium decodes them natively. YouTube streams are AAC/Opus, so on this
  machine the first play of a track is an automatic transcode (one silent retry, then local
  MP3) instead of instant stream matching. On machines with working Media Foundation codecs
  the same build behaves as before (direct stream first, transcode in the background).
- Local imports of AAC/M4A files are still copied into the library as-is and play only where
  the webview can decode them; import as MP3/WAV/FLAC for this machine.
- SoundCloud direct-resolution URLs (preview transcodings) are short-lived and MP3-only;
  gated tracks keep returning yt-dlp errors exactly like upstream, and SoundCloud HLS-only
  tracks fall back to yt-dlp instead of the direct path.

## Build

```bat
npm install
npm run assemble   # src/renderer + assets + vendor -> web/
npm run build      # cargo tauri build -> NSIS installer
```

`scripts/assemble.ps1` flattens the web assets and rewrites `../../assets/` references;
edits to the renderer must happen in `src/renderer`, then re-run assemble.

## Data location

`%LOCALAPPDATA%\Riff Rust\` (library/, tracks/, local-covers/, logs/, storage.json, config.json, library.json).
A custom data directory, set via the app settings or `set_data_dir`, is honoured from the next
launch through a fixed-location bootstrap pointer at `%LOCALAPPDATA%\Riff Rust Boot\data-dir.json`
(macOS/Linux equivalents documented in `util.rs`). The pointer lives outside the data dir on
purpose; if it goes missing or unusable, the app falls back to the default data dir with one
WARN and never deletes anything.
