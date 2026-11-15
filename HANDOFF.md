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
| `src-tauri/src/` | Rust: `main.rs` (window/tray/boot), `server.rs` (embedded API), `tools.rs` (yt-dlp/ffmpeg auto-install), `commands.rs` (Tauri IPC), `library.rs` (import/tags via lofty), `ytm.rs` (YouTube Music inner-API search), `state.rs` (shared client/caches), `util.rs` |
| `scripts/assemble.ps1` | Flattens renderer + assets + vendor into `web/`, rewrites `../../assets/` refs, injects `electron-shim.js` into the HTML. **Edit sources in `src/renderer`, never `web/` (build artifact).** |
| `scripts/clean.ps1` | Disk cleanup (`npm run clean`, `npm run clean:all`). |
| `scripts/measure.ps1` | RAM/CPU sampling (`npm run measure`). |
| `PORTING.md` | Authoritative feature status. Keep it updated. |
| `release/` | Installer artifacts (gitignored). |

## Build & release

```bat
npm install
npm run assemble
npm run build    # -> src-tauri/target/release/bundle/nsis/
```

Release profile (`src-tauri/Cargo.toml`): `lto = true`, `codegen-units = 1`, `opt-level = 3`,
`panic = "abort"`, `strip = true` — a cold build is slow (~15–25 min), but the shipped binary is
maximally optimized. Don't bump `opt-level` past 3; `s` saves little and costs hot paths.

Windows needs: Node 18+, Rust (MSVC), WebView2 (already on Win10/11). First run auto-downloads yt-dlp; ffmpeg via `winget install Gyan.FFmpeg`.

`npm run clean` removes `web/`, `release/`, app logs, and `src-tauri/target/debug/`
(check/test artifacts, usually the biggest folder); `npm run clean:all` also removes
`node_modules/` and all of `src-tauri/target/` (next build is cold, ~10–15 min).
`npm run measure` reports root and whole-process-tree RAM/CPU (WebView2 children included).

Release process used for v0.1.x: bump `version` in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`; commit; tag; `gh release create` with the NSIS `.exe`. Repo: `ziyyhhk/riff-rust` (public, non-prerelease "Latest" releases).

## Hard-won gotchas

1. The axum listener must be **nonblocking before `TcpListener::from_std`** or all requests hang (fixed).
2. The shim needs `core:event:default` + `dialog:default` in `src-tauri/capabilities/default.json` or `__TAURI__.event.listen` and the file dialogs fail with ACL errors (fixed).
3. Playback CORS: the webview is cross-origin to `127.0.0.1:<port>`; all `/api/*` responses need `Access-Control-Allow-Origin: *` (tower-http CorsLayer, done). Removing it breaks streaming and the X-Riff-Token preflight.
4. `Cargo.lock` is committed; `cargo clean` costs ~10–15 min full rebuild, so think before cleaning the cache.
5. The tutorial marker is `%LOCALAPPDATA%\Riff Rust\.rifftutorial` — **existence-only** (matching upstream); delete it to re-test the first-run wizard.
6. `src-tauri/gen/` is build-generated (schemas) — never commit it (gitignored).
7. Smoke test: launch the release exe, read `%LOCALAPPDATA%\Riff Rust\logs\riff-rust.log` ("serve thread started"…"axum serve starting"), and curl `127.0.0.1:<port>` endpoints. The renderer will `/api/youtube-account-info` at boot (stub = normal).
8. Axum handlers must be `Send`. Do not hold `&Request<Body>` across an `await` — extract the
   `Range` header first and pass `Option<&str>` (the request body type is not `Sync`).
9. Global-shortcut accelerator names must match `global-hotkey`'s parser: `MediaTrackNext`,
   `MediaTrackPrevious`, not `MediaNextTrack`. Registration failures are logged as `WARN`.
10. Window geometry must use the monitor **work area**, not `monitor.size()` (which includes the
    taskbar). `startup_geometry()` calls Win32 `GetMonitorInfoW` and stores logical/DIP bounds in
    `window-state.json`, same shape as Electron. Removing this makes the window ~40 px too tall.
11. The onboarding result is pulled by the shim (`take_welcome_result`) when `onWelcomeResult`
    registers, not emitted on a timer. Tauri's `on_page_load` is builder-only, so a fixed sleep
    can fire before `app.js` listens on a slow first launch.
12. `tools.rs` runs `ensureTools` at **every boot** (like upstream): missing binaries download
    automatically with SHA-256 verification and the renderer closes the required-tools modal when
    `/api/tools/status` reports both. Do not add a "ffmpeg optional on Windows" shortcut; it forks
    behaviour away from upstream. If the modal spins forever, check `riff-rust.log` for the install
    error and `progress.status` from `/api/tools/status` (`downloading|done|error`).
13. `tauri::generate_context!()` panics with a bare "proc macro panicked" when `web/` (the
    `frontendDist`) does not exist — e.g. right after `npm run clean:all`. Run `npm run assemble`
    before `cargo build`/`tauri build`; the error message gives no hint that the frontend is
    missing.
14. YTM's `FEmusic_charts` page contains **no track rows** — only `musicTwoRowItemRenderer`
    "chart cards" (`subtitle` contains "Chart • YouTube Charts"). `/api/top-chart` parses the
    cards, picks the best playlist (Top 100 > Trending > Top 50), and browses that playlist
    (`musicResponsiveListItemRenderer` rows, with the artist in flex column 1). Parsing rows
    directly from the charts page yields zero tracks.
15. `npm install`-time version bumps must not be done with blanket `x.y.z` string replace —
    it corrupted `Cargo.lock` (`hyper-util`/`windows-sys` patch versions). Bump only the root
    package versions, then let cargo sync the lockfile.
16. Any `Command::new` on Windows must use `CREATE_NO_WINDOW` (`util::hide_console`) or the
    GUI app's console children flash black cmd windows (user-visible bug, fixed in v0.1.4).
    Audit new spawns — Explorer/`open`/`xdg-open` are the exceptions (they ARE the UI).
17. Playback order in `stream_info` (get it backwards and code=4 returns):
    local library → playable cache (mp3/wav/flac) → any cached track re-encoded to mp3
    (source m4a deleted) → SoundCloud direct resolve (progressive MP3, ~1 s) → stream
    cache (10 min) → **VISIONOS inner-client / v1/player with the current player JS
    `signatureTimestamp`** (unrestricted byte ranges; ~0.3 s when warm, ~2-3 s cold;
    cached 24 h in `visionos_context`) → iOS inner-client (only serves ~256 KB) →
    yt-dlp fallback. The renderer's second retry passes `forceYtDlp=1`; it transcodes
    into `tracks/<id>.mp3` via `ensure_mp3_cached` (single-flight — prefetch and
    recovery never download the same track twice) and returns `/api/local-track?id=<id>`
    (guaranteed decode). The VISIONOS inner client only returns a playable URL when
    the request carries both the watch page's visitorData and the current player JS
    `signatureTimestamp` — the request needs bpctr=9999999999&has_verified=1 on the
    watch page. `/api/prefetch` + the renderer's next-track prefetch produce the MP3 while the
    current track plays; `/api/cached` lets playTrack swap a stale proxy URL for the
    local file. Album/Daily Mix rows can carry synthetic ids (`alb_dm_...`, numeric
    Deezer ids); when the id is not an 11-char YouTube id and no usable URL is
    present, `/api/stream-info` resolves the row with the fast inner-API search
    using the `q`/`title`/`artist` the renderer sends (`best_search_hit`), instead of
    asking YouTube for `watch?v=alb_...` (that was the "Track can't be loaded"
    flood and the reason rows got skipped/replaced). UI scale: the renderer's
    UI-scale slider scales `--font-scale`, `--sidebar-w` and `--engine-w` in
    place. Native or raw CSS zoom
    (`document.documentElement.style.zoom` / `document.body.style.zoom`) shrinks the
    page into a dead empty corner of the window and is NOT used.
18. SoundCloud has no auth-free "API key" in the repo: the public `client_id` is scraped
    from the front page's `a-v2.sndcdn.com/assets/*.js` bundles (cached 12 h in `state.rs`).
    `GET api-v2.soundcloud.com/search/tracks?q=...&client_id=...` works for search (~0.6 s);
    `/resolve` returns `media.transcodings[]`. Progressive MP3 (protocol `progressive`,
    mime audio/mpeg) is still preferred; newer uploads only expose HLS (`aac_160k` /
    `aac_96k` / `abr_sq`), so `resolve_stream` also returns the `playlist.m3u8` with
    `is_hls: true`. Its CDN sends `Access-Control-Allow-Origin: *`, so hls.js in the
    renderer plays it directly. If every rendition fails, the yt-dlp path takes over.
19. googlevideo does not serve ffmpeg's TLS stack (Schannel in the gyan.dev Windows build):
    a direct `ffmpeg -i <googlevideo url>` fails with `HTTP 403`, even with yt-dlp's URL and
    even with matching User-Agents, while reqwest (rustls) and urllib fetch the same URLs
    fine. Never let ffmpeg touch the network: download to a temp file with reqwest
    (`download_to_file`), transcode from the file, delete the temp. This is why the mp3
    cache transcode is a two-step process. Also: ffmpeg infers the output container from the
    file extension, so cache temp files for transcoding must end in `.mp3` or be forced with
    `-f mp3` (we pass `-f mp3`), or ffmpeg exits instantly with "Unable to find a suitable
    output format". And capture ffmpeg's stderr to the app log on failure; a silent
    `.output()` hides the one line that explains everything.
20. `find_track_by_id` probes playable extensions first everywhere (library then cache,
    then unplayable ones). Do not "simplify" it back to a single probe loop, or stale
    `.m4a` caches will shadow the `.mp3` transcodes and unplayable files resurface.
21. The renderer's failed-track blocklist (`riff_failed_tracks`) is versioned:
    `riff_failed_tracks_schema === '2'` clears it once on upgrade. Do not record a
    failure by mutating the loaded queue/album/mixes (that replaced rows mid-play and
    looked like the app skipping to other music). `playTrack` and
    `recoverCurrentStreamOnce` retry twice (the second pass uses `forceYtDlp=1`) before
    any failure is recorded or shown.

## Done (see PORTING.md)

First-run onboarding wizard (upstream-identical), launcher Start-menu shortcut (Windows), local library (import/tags/covers/range streaming), YouTube Music search fast path + SoundCloud search (direct API with yt-dlp fallback), unified hybrid search (YouTube Music + SoundCloud interleaved 2:1, `rifflibrary`), MP3-first stream caching + transcode-on-decode-failure (v0.1.8), streaming + downloads via yt-dlp, tools auto-install (yt-dlp/ffmpeg with checksum verification and progress, `src-tauri/src/tools.rs`), LRCLIB + custom lyrics, public Spotify playlist import (anonymous embed-token pathfinder), YouTube/SoundCloud playlist import, tray + close-to-tray, cache/data-dir management, settings persistence (`storage.json`), media keys + global shortcuts, mini-player with expandable lyrics, window-visibility events for pausing background work, log rotation, disk cleanup/measure scripts, Discord Rich Presence (src-tauri/src/discord_rpc.rs, IPC pipe), karaoke word-level lyrics (/api/lyrics-words via NetEase + LRC [offset:] honoured + 24h cache), upstream window startup parity (work-area sizing, saved geometry, hidden until load), discovery (`src-tauri/src/discovery.rs`: vibe/top-chart/trending/hot/artist — home page sections now populate). Audio streams play through a loopback audio-proxy (upstream shape `streamUrl`/`proxyUrl`; the direct googlevideo URL breaks the Web Audio graph) and resolved streams are cached to `tracks/` as MP3 (re-encoded on the way in) for repeat plays, with on-demand transcode of legacy m4a/webm caches and `forceYtDlp=1` local-MP3 recovery on decode failure.


















## Not done (in priority order if continuing parity)

1. Google/YouTube sign-in & import (OAuth PKCE + account file) - upstream: google-oauth.js, youtube-data.js
2. Spotify account integration (auth, library) - import works, account features don't
3. Apple Music / Deezer / Yandex / TikTok / VK import
4. Plugin execution sandbox, auto-updater (no feed yet)


## Honesty rules

- Don't claim parity. "Same UI" is true; "same features" is not yet.
- Memory/CPU claims need measurements. Use 
pm run measure on the same playback scenario in
  upstream Riff and Riff Rust before publishing a number; the architecture explains the
  direction but does not prove a figure.
- Keep PORTING.md current; the owner reads it.
