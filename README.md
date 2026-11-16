<p align="center">
  <img src="banner.png" alt="Riff Rust, a desktop music player and audio suite for YouTube Music, SoundCloud, and local files" width="100%">
</p>

<p align="center">
  <img src="https://cdn.jsdelivr.net/gh/devicons/devicon@latest/icons/windows8/windows8-original.svg" width="22" height="22" alt="Windows">&nbsp;&nbsp;
  <img src="https://cdn.jsdelivr.net/gh/devicons/devicon@latest/icons/apple/apple-original.svg" width="22" height="22" alt="macOS">&nbsp;&nbsp;
  <img src="https://cdn.jsdelivr.net/gh/devicons/devicon@latest/icons/linux/linux-original.svg" width="22" height="22" alt="Linux">&nbsp;&nbsp;
  <img src="https://cdn.simpleicons.org/ios/9ca3af" width="22" height="22" alt="iOS">&nbsp;&nbsp;
  <img src="https://cdn.jsdelivr.net/gh/devicons/devicon@latest/icons/android/android-original.svg" width="22" height="22" alt="Android">
</p>

<h1 align="center">
  <img src="github/readme/emoj2i.gif" width="48" height="48" align="absmiddle" alt="emoji">&nbsp;Riff Rust
</h1>

> **Riff Rust** is a from scratch [Rust + Tauri](https://tauri.app) rebuild of [Riff](https://github.com/rootscripts/riff) (Electron + vanilla JavaScript, MIT).
> Same UI, rebuilt backend: a native WebView instead of bundled Chromium, and a Rust HTTP API on localhost instead of a Node server, aiming for far lower RAM and CPU use.
> It is **not** affiliated with the upstream Riff team; it is a community rebuild. Feature parity is a work in progress, see [PORTING.md](PORTING.md) for the exact status of every feature.
> Original Riff copyright belongs to its authors; this fork remains MIT licensed (see [LICENSE](LICENSE)).

<p align="center">
  <b>A free, open source desktop music player for YouTube Music, SoundCloud, and local files.</b><br>
  Built with Material Design 3, no tracking, and no account needed.
</p>

<p align="center">
  <a href="https://github.com/ziyyhhk/riff-rust/releases">
    <img src="https://img.shields.io/badge/Download-C9A3F2?style=for-the-badge&logo=download&logoColor=1E1A2B" height="40" alt="Download">
  </a>
  &nbsp;
  <a href="https://riff.hs.vc">
    <img src="https://img.shields.io/badge/Website-1F2328?style=for-the-badge&logo=open-webui&logoColor=white" height="40" alt="Visit Website">
  </a>
</p>

<p align="center">
  <a href="#core-features">Features</a> &nbsp;â€¢&nbsp;
  <a href="#built-in-audio-suite">Audio Suite</a> &nbsp;â€¢&nbsp;
  <a href="#material-design-3-expressive-ui">Interface</a> &nbsp;â€¢&nbsp;
  <a href="#download-and-install">Install</a> &nbsp;â€¢&nbsp;
  <a href="#faq">FAQ</a> &nbsp;â€¢&nbsp;
  <a href="#build-from-source">Build</a>
</p>

<p align="center">
  <a href="https://github.com/ziyyhhk/riff-rust/stargazers"><img src="https://img.shields.io/github/stars/ziyyhhk/riff-rust?style=flat-square&labelColor=16191C&color=CAD3F5" alt="Stars"></a>
  <a href="https://github.com/ziyyhhk/riff-rust/commits/main"><img src="https://img.shields.io/github/last-commit/ziyyhhk/riff-rust?style=flat-square&labelColor=16191C&color=94C9FF" alt="Last Commit"></a>
  <a href="https://github.com/ziyyhhk/riff-rust/blob/main/LICENSE"><img src="https://img.shields.io/github/license/ziyyhhk/riff-rust?style=flat-square&labelColor=16191C&color=8EEDD8" alt="License"></a>
  <a href="https://ko-fi.com/rootlesszen"><img src="https://img.shields.io/badge/Kofi-support-FF5E5B?style=flat-square&logo=kofi&logoColor=white&labelColor=16191C" alt="Support on Kofi"></a>
</p>

> [!IMPORTANT]
> **Contributions and Pull Requests Policy**
> This project is actively developed and maintained exclusively by its core team. **Public Pull Requests are disabled and will not be accepted.**
>
> * **Bugs:** Please report them via the Issues tab using the provided bug report template.
> * **Custom features and experiments:** You are welcome to **fork the repository** and maintain your own builds.
> * Please do not open issues requesting to enable PRs.
> * We do not accept any users to our core team.

## What is Riff Rust?

Riff Rust is an open source, telemetry free desktop music app and player for **Windows, Linux, and macOS**. It unifies playback across **YouTube Music**, **YouTube**, **SoundCloud**, and offline local media into a single high performance interface without ads, subscriptions, or login constraints.

Riff Rust ships no Chromium runtime. It renders in the OS WebView2 and serves a native Rust API on `127.0.0.1`, replacing the Node server and Electron shell downstream of the upstream Riff project. The result is the exact same interface, with far less RAM and CPU pressure, and a single small installer. Project code is distributed under the **MIT license**; all configuration, cached streams, and playback history remain strictly on the local machine.

## Core Features

* High quality streaming with **YouTube Music, YouTube, and SoundCloud** without third party advertisements or account requirements
* **Offline playback with full local library support** for high resolution formats including FLAC, WAV, MP3, OGG, and M4A
* **Easy built in audio editor**, trimming, effects, and a ten band equalizer, plus real time DSP effects (speed, pitch, reverb, distortion)
* **One click multi platform playlist migration**: direct import and resolution of public playlists from Spotify, Apple Music, Deezer, YouTube Music, and SoundCloud
* **Material Design 3 UI**: dynamic color extraction from album artwork, deep UI theming, responsive layout presets, and an integrated CSS editor
* **Hardware accelerated visuals**: canvas and WebGL visualizers including real time frequency analyzers, an interactive squiggly progress scrubber, and synchronized karaoke lyrics
* **Discord Rich Presence**: real time playback status reporting track metadata, artist info, and elapsed time to Discord
* **Zero telemetry**: no tracking identifiers, analytics pings, or background data collection
* **Fullscreen mode** with fancy lyrics animation
* **Albums**: see all of your favorite artist albums
* **Customization**: built in theme editor, configure everything, and a CSS editor, with animated backgrounds and interface opacity sliders
* **Plugins**: add a plugin to change how Riff Rust works, or add new functionality entirely

## Built in Audio Suite

Riff Rust includes an integrated audio engineering toolkit accessible directly within the player interface:

### Visual Audio Editor
* **Lossless trimming**: define start and end boundaries to trim tracks without transcoding loss
* **Region extraction**: select and isolate specific audio segments to save as standalone files
* **Non destructive processing**: edit operations produce independent files while preserving the original source media untouched

### Ten Band Parametric Equalizer
* Dedicated frequency sliders covering 32 Hz through 16 kHz with pre configured acoustic profiles (Bass Boost, Vocal Clarity, Electronic, Flat, Rock)
* Per channel gain adjustments with real time Web Audio API signal processing

### Real Time DSP Effects Chain
* **Tempo control**: dynamically speed up or slow down playback rate without altering pitch
* **Pitch shifting**: independent semitone pitch adjustment for rehearsal and tuning
* **Reverb and spatial effects**: algorithmic convolution and room size simulation for spatial enhancement
* **Distortion and overdrive**: adjustable wave shaping saturation and drive harmonics

### Offline Library and Download Pipeline
* Download streams from YouTube Music, YouTube, and SoundCloud directly to local disk storage (MP3, FLAC, Opus, WAV)
* Automatic ID3 and metadata tagging, including high resolution cover art embedding
* Complete offline availability with full playlist compilation and search indexing

## Material Design 3 Expressive UI

Riff Rust implements the Material Design 3 (M3 Expressive) specification:

* **Dynamic color extraction**: generates responsive UI tonal palettes on the fly based on the dominant and secondary hues of active album art
* **Deep UI customization**: configurable banner backdrops, mesh gradients, blur filters, and typography controls
* **Integrated CSS editor**: built in developer console allowing direct CSS overrides and custom style injection to personalize every aspect of the interface
* **Optimized layout modes**: compact mini player layout, expanded desktop view, and fullscreen TV and karaoke mode with synchronized lyrics rendering

## Multi Platform Playlist Migration

Migrate existing music libraries between platforms in a single operation:

* Import public playlist URLs from **Spotify** (including playlists with 100+ tracks via automated pagination)
* Import public playlists from **Deezer**, and public playlists and albums from **Apple Music**, **SoundCloud**, and **YouTube / YouTube Music**
* Platform aware search resolution: matches imported track metadata against the active account provider to construct immediate local playlists

## Synchronized Lyrics Engine

* Automatic multi provider lyrics lookup matching artist and title metadata
* Native support for synchronized LRC files, plain text lyrics, and dynamic online timestamp alignment
* Inline time offset adjustments to manually synchronize lyrics with live streams
* Multi language interface and lyrics translation directly inside the player

## Download and install

Pre built binaries are available on the [Releases page](https://github.com/ziyyhhk/riff-rust/releases).

Standard releases include all necessary dependencies (bundled yt-dlp and ffmpeg binaries). No secondary runtime configuration is required.

On Windows, the installer lets you choose the installation directory. Uninstalling Riff Rust does not delete your application data.

### Linux

Linux support follows the same principles as the desktop builds. Use your distributions preferred format when one is published, or build from source below. WebView2 is replaced by the native WebKitGTK on Linux.

### macOS

macOS support is currently experimental and compiled via continuous integration.

1. Download the appropriate disk image:
   * `Riff-Rust-<version>-mac-arm64.dmg` for Apple Silicon (M1, M2, M3, M4)
   * `Riff-Rust-<version>-mac-x64.dmg` for Intel based hardware
2. Open the disk image and drag Riff Rust to your `Applications` directory.
3. Riff Rust is ad hoc signed but not notarized, so the first launch is blocked. Right click `Riff Rust.app` in `Applications`, select **Open**, and confirm the prompt. On macOS 15 and later, open it once, then go to **System Settings â†’ Privacy and Security** and click **Open Anyway**.
4. If macOS still flags the bundle as damaged, execute the following command in Terminal:
   ```bash
   xattr -cr /Applications/Riff\ Rust.app
   ```
5. Riff Rust uses yt-dlp for streaming. The tools are downloaded on first launch on other platforms; on macOS install them with Homebrew:
   ```bash
   brew install yt-dlp ffmpeg
   ```

macOS application data and local indexes are stored in `~/Library/Application Support/Riff Rust`.

## Build from source

Requires [Node.js 18+](https://nodejs.org), the [Rust toolchain](https://rustup.rs), and the Tauri platform prerequisites (on Windows: WebView2, already present on Windows 10 and 11).

```bash
git clone https://github.com/ziyyhhk/riff-rust
cd riff-rust
npm install
npm run assemble   # flatten src/renderer + assets + vendor into web/
npm run build      # cargo tauri build into a bundle under src-tauri/target/release/bundle/
```

Runtime needs `yt-dlp` and `ffmpeg` (both auto download on first run from official releases, verified by SHA-256; about 200 MB total). Streaming works without ffmpeg.

The exact bundle produced depends on the host platform; on Windows the output is an NSIS installer.

## Memory usage

Upstream Riff (Electron) typically uses ~180 MB steady state. Riff Rust ships no bundled Chromium; it uses the OS WebView2 instead. A measured whole process tree (`riff-rust.exe` plus all `msedgewebview2.exe` children) idling at the home screen used ~310 MB private bytes and ~517 MB working set on one Windows machine, roughly the same order as Electron; the clear win is disk (a single small installer versus a ~200 MB download) and startup time. No side by side numbers against upstream on the same playback scenario have been published yet; run `npm run measure` yourself.

## FAQ

**Is Riff Rust free?**
Yes. Riff Rust is free software published under the permissive MIT license. There are no subscriptions, paywalls, or premium tiers.

**Does Riff Rust display advertisements?**
No. Streams from YouTube Music, YouTube, and SoundCloud are resolved directly and play without advertisements.

**Is an account or sign in required?**
No account is required for playback or public playlist imports. Optional Google sign in lets you import your own YouTube likes and playlists; Riff Rust uses Google OAuth and the YouTube Data API for that feature. Telemetry and analytic tracking are strictly omitted.

**Which operating systems are supported?**
Windows 10 and 11, modern Linux distributions, and macOS (experimental).

**Can Riff Rust function as an offline local music player?**
Yes. Riff Rust natively indexes local audio files (including lossless FLAC, WAV, MP3, and OGG), reads standard metadata tags and embedded artwork, and manages playlists without altering the underlying disk files.

**Can I use Riff Rust entirely offline?**
Yes. Downloaded tracks, saved audio segments, and indexed local libraries function without an internet connection.

**How does playlist migration work?**
Paste any public Spotify, Apple Music, Deezer, YouTube Music, or SoundCloud playlist link. Riff Rust extracts track titles and artists, resolves streams on your active platform, and saves the result as an editable local playlist.

**Does Riff Rust support synchronized lyrics?**
Yes. Riff Rust supports online synchronized lyrics, local LRC files, manual timing adjustments, and real time synchronized karaoke display.

**Does Riff Rust integrate with Discord?**
Yes. Native Discord Rich Presence displays the currently playing track title, artist, elapsed time, and a platform indicator.

**Are there official mobile builds?**
No official mobile releases exist. Third party community ports may appear in the future; this repository does not distribute them.

## Support

You can Support ongoing development via [Kofi](https://ko-fi.com/rootlesszen). Repository stars on GitHub are also appreciated.

## Disclaimer

Riff Rust is developed for educational purposes and personal media management. Users are responsible for adhering to the terms of service of third party platforms and applicable local copyright regulations.

## License

[MIT](LICENSE)

***
<sub>Keywords: free music player, YouTube Music desktop app, SoundCloud desktop player, ad free music player, Material Design 3, M3 Expressive, visual audio editor, lossless audio trimmer, ten band equalizer, DSP effects, tempo changer, pitch shift, FLAC player, WAV playback, Spotify playlist import, Apple Music playlist migration, Rust music player, Tauri music player, WebGL audio visualizer, Discord Rich Presence, open source music player, MIT license, telemetry free desktop player.</sub>

