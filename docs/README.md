# Riff Rust docs

Everything that is not code lives here, so the repository root stays clean.

| Path | What it is |
|------|------------|
| `releases/` | Per-version release notes (`v0.1.11.md` … `v0.1.21.md`). |
| `HANDOFF.md` | Session handoff: read this first when picking the project up. |
| `PORTING.md` | Authoritative feature-by-feature porting status vs upstream Riff. |
| `img/` | README / branding artwork (hero, banner, social preview, emoji). |

## Repository layout

```
.github/workflows/   CI (release installer build)
assets/              app assets copied into web/ by assemble
bin/                 runtime helper binaries notes
docs/                this folder (notes, docs, artwork)
plugin/              bundled example plugin
scripts/             assemble / clean / measure PowerShell helpers
src/renderer/        the frontend (HTML/CSS/JS) served by the webview
src-tauri/           Rust backend (Tauri shell + localhost HTTP API)
vendor/              vendored third-party frontend code (monaco)
```

Build helpers live in `scripts/` and are wired through `package.json`:
`npm run assemble`, `npm run build`, `npm run clean`, `npm run measure`.
