//! Background installer for yt-dlp and ffmpeg.
//!
//! Port of upstream Riff's `tools.js`: same release URLs, same SHA-256 checksum
//! verification, same Windows extraction strategy (PowerShell -> Expand-Archive
//! -> tar), same `ensureTools` semantics. The renderer polls `/api/tools/status`
//! every 500 ms while the install modal is open, so progress must be observable
//! through `AppState::tool_install`.
//!
//! Install on boot is identical to upstream: if a binary is missing it is
//! fetched automatically, and the modal closes itself once `tools/status`
//! reports both binaries (or `ffmpegOptional` on macOS).

use crate::state::AppState;
use crate::util;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) RiffRust/0.1.2";
const FFMPEG_BASE: &str = "https://github.com/yt-dlp/FFmpeg-Builds/releases/latest/download";
const YT_DLP_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";

fn ffmpeg_asset() -> Option<&'static str> {
    if cfg!(target_os = "windows") {
        return Some(if cfg!(target_arch = "aarch64") {
            "ffmpeg-master-latest-winarm64-gpl.zip"
        } else {
            "ffmpeg-master-latest-win64-gpl.zip"
        });
    }
    if cfg!(target_os = "linux") {
        return Some(if cfg!(target_arch = "aarch64") {
            "ffmpeg-master-latest-linuxarm64-gpl.tar.xz"
        } else {
            "ffmpeg-master-latest-linux64-gpl.tar.xz"
        });
    }
    None
}

fn ytdlp_asset() -> &'static str {
    if cfg!(target_os = "windows") {
        if cfg!(target_arch = "aarch64") {
            "yt-dlp_arm64.exe"
        } else if cfg!(target_arch = "x86") {
            "yt-dlp_x86.exe"
        } else {
            "yt-dlp.exe"
        }
    } else if cfg!(target_os = "macos") {
        "yt-dlp_macos"
    } else if cfg!(target_arch = "aarch64") {
        "yt-dlp_linux_aarch64"
    } else {
        "yt-dlp_linux"
    }
}

/// Same parser as upstream `expectedHash`: `<sha256>  <name>` lines, optional
/// `*` binary marker or leading `./`.
fn expected_hash(sums: &str, file_name: &str) -> Option<String> {
    for raw in sums.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(hash) = parts.next() else { continue };
        let Some(rest) = parts.next() else { continue };
        let name = rest.trim_start_matches('*').trim_start_matches("./");
        if name == file_name
            && hash.len() == 64
            && hash.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Some(hash.to_lowercase());
        }
    }
    None
}

fn sha256_hex(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(path)?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn http_blocking(url: &str, timeout_secs: u64) -> Result<reqwest::blocking::Response, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", response.status().as_u16()));
    }
    Ok(response)
}

fn http_get_text(url: &str) -> Result<String, String> {
    http_blocking(url, 60)?
        .text()
        .map_err(|e| format!("GET {url}: {e}"))
}

fn bin_exe(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Download to `dest`, reporting 0-99% into `state` (100% on success). Returns
/// the downloaded file's SHA-256 hex, like upstream's `downloadFile`.
fn download_with_progress(
    state: &Arc<AppState>,
    url: &str,
    dest: &Path,
) -> Result<String, String> {
    let _ = fs::remove_file(dest);
    let mut response = http_blocking(url, 900)?;
    let total = response.content_length().unwrap_or(0);
    let mut out = fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 64 * 1024];
    let mut done: u64 = 0;
    loop {
        let n = response.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if total > 0 {
            let pct = ((done as f64 / total as f64) * 99.0).min(99.0) as u8;
            state.tool_install().percent = pct;
        }
    }
    out.flush().map_err(|e| e.to_string())?;
    drop(out);
    state.tool_install().percent = 100;
    sha256_hex(dest).map_err(|e| e.to_string())
}

/// Depth-first search for any of `names` under `dir`.
fn find_file(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).ok()? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                stack.push(path);
                continue;
            }
            let matches = entry
                .file_name()
                .to_str()
                .map(|name| names.iter().any(|want| name == *want))
                .unwrap_or(false);
            if matches {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(target_os = "windows")]
fn extract_zip(archive: &Path, dest: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let archive_s = archive.to_string_lossy().replace('\'', "''");
    let dest_s = dest.to_string_lossy().replace('\'', "''");
    let commands: Vec<Vec<String>> = vec![
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-ExecutionPolicy".into(),
            "Bypass".into(),
            "-Command".into(),
            format!(
                "Add-Type -AssemblyName System.IO.Compression.FileSystem; [System.IO.Compression.ZipFile]::ExtractToDirectory('{archive_s}', '{dest_s}')"
            ),
        ],
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-Command".into(),
            format!("Expand-Archive -LiteralPath '{archive_s}' -DestinationPath '{dest_s}' -Force"),
        ],
    ];
    for args in commands {
        match std::process::Command::new("powershell.exe")
            .args(&args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            Ok(out) if out.status.success() => return Ok(()),
            Ok(out) => util::log_line(
                "WARN",
                &format!(
                    "powershell extract failed: {:?}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            ),
            Err(e) => util::log_line("WARN", &format!("powershell extract failed to start: {e}")),
        }
    }

    let system_tar = PathBuf::from(r"C:\Windows\System32\tar.exe");
    let tar = if system_tar.is_file() {
        system_tar
    } else {
        PathBuf::from("tar.exe")
    };
    match std::process::Command::new(&tar)
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "tar.exe extract failed: {:?}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("tar.exe extract failed: {e}")),
    }
}

#[cfg(not(target_os = "windows"))]
fn extract_tar_xz(archive: &Path, dest: &Path) -> Result<(), String> {
    let status = std::process::Command::new("tar")
        .arg("-xJf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .map_err(|e| format!("tar failed to start: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tar failed with exit status {status}"))
    }
}

fn install_yt_dlp(state: &Arc<AppState>) -> Result<(), String> {
    let asset = ytdlp_asset();
    let sums = http_get_text(&format!("{YT_DLP_BASE}/SHA2-256SUMS"))?;
    let want = expected_hash(&sums, asset)
        .ok_or_else(|| format!("no checksum published for {asset}"))?;

    let bin_dir = util::bin_dir();
    let target = bin_dir.join(asset);
    let tmp = bin_dir.join(format!("{asset}.download"));
    let got = download_with_progress(state, &format!("{YT_DLP_BASE}/{asset}"), &tmp)?;
    if got != want {
        let _ = fs::remove_file(&tmp);
        return Err("yt-dlp checksum mismatch".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755));
    }
    if fs::rename(&tmp, &target).is_err() {
        fs::copy(&tmp, &target)
            .map_err(|e| format!("could not install yt-dlp to {}: {e}", target.display()))?;
        let _ = fs::remove_file(&tmp);
    }
    util::log_line("INFO", &format!("yt-dlp installed to {}", target.display()));
    Ok(())
}

fn install_ffmpeg(state: &Arc<AppState>) -> Result<(), String> {
    let Some(asset) = ffmpeg_asset() else {
        // macOS: ffmpeg stays optional, exactly like upstream.
        return Ok(());
    };
    let sums = http_get_text(&format!("{FFMPEG_BASE}/checksums.sha256"))?;
    let want = expected_hash(&sums, asset)
        .ok_or_else(|| format!("no checksum published for {asset}"))?;

    let temp_dir = std::env::temp_dir();
    let archive = temp_dir.join(format!("riff-{asset}"));
    let work = temp_dir.join(format!("riff-ffmpeg-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);

    let got = download_with_progress(state, &format!("{FFMPEG_BASE}/{asset}"), &archive)?;
    if got != want {
        let _ = fs::remove_file(&archive);
        return Err("ffmpeg checksum mismatch".to_string());
    }

    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    extract_zip(&archive, &work)?;
    #[cfg(not(target_os = "windows"))]
    extract_tar_xz(&archive, &work)?;

    let bin_dir = util::bin_dir();
    let wanted = [bin_exe("ffmpeg"), bin_exe("ffprobe")];
    for name in &wanted {
        let src = find_file(&work, &[name.as_str()])
            .ok_or_else(|| format!("{name} not found inside {asset}"))?;
        let dest = bin_dir.join(name);
        fs::copy(&src, &dest)
            .map_err(|e| format!("could not install {name} to {}: {e}", dest.display()))?;
    }
    let _ = fs::remove_dir_all(&work);
    let _ = fs::remove_file(&archive);
    util::log_line(
        "INFO",
        &format!("ffmpeg installed to {}", bin_dir.display()),
    );
    Ok(())
}

#[cfg(target_os = "windows")]
fn try_winget_install(package_id: &str) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let mut child = match std::process::Command::new("winget")
        .args([
            "install",
            "--id",
            package_id,
            "-e",
            "--accept-source-agreements",
            "--accept-package-agreements",
            "--silent",
            "--disable-interactivity",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            util::log_line("ERROR", &format!("winget spawn failed: {e}"));
            return false;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(_) => return false,
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn try_winget_install(_package_id: &str) -> bool {
    false
}

fn set_downloading(state: &AppState, tool: &str) {
    let mut slot = state.tool_install();
    slot.status = "downloading".to_string();
    slot.tool = tool.to_string();
    slot.percent = 0;
    slot.error.clear();
}

fn begin_and_run(state: Arc<AppState>) -> Result<(), String> {
    let bin_dir = util::bin_dir();
    fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;

    if util::resolve_binary("yt-dlp").is_none() {
        set_downloading(&state, "yt-dlp");
        if let Err(e) = install_yt_dlp(&state) {
            #[cfg(target_os = "windows")]
            {
                util::log_line("WARN", &format!("direct yt-dlp install failed: {e}"));
                if !try_winget_install("yt-dlp.yt-dlp") || util::resolve_binary("yt-dlp").is_none()
                {
                    return Err(e);
                }
            }
            #[cfg(not(target_os = "windows"))]
            return Err(e);
        }
    }

    if util::resolve_binary("ffmpeg").is_none() && ffmpeg_asset().is_some() {
        set_downloading(&state, "ffmpeg");
        if let Err(e) = install_ffmpeg(&state) {
            #[cfg(target_os = "windows")]
            {
                util::log_line("WARN", &format!("direct ffmpeg install failed: {e}"));
                if !try_winget_install("Gyan.FFmpeg") || util::resolve_binary("ffmpeg").is_none()
                {
                    return Err(e);
                }
            }
            #[cfg(not(target_os = "windows"))]
            return Err(e);
        }
    }

    state.tool_install().ffmpeg_error.clear();
    Ok(())
}

/// Kick off (or no-op) the background installer. Safe to call on every boot and
/// from `/api/tools/install`; only one run happens at a time.
pub fn ensure_tools(state: Arc<AppState>) {
    {
        let mut slot = state.tool_install();
        if slot.status == "downloading" {
            return;
        }
        slot.status = "downloading".to_string();
        slot.tool.clear();
        slot.percent = 0;
        slot.error.clear();
    }
    std::thread::spawn(move || {
        match begin_and_run(state.clone()) {
            Ok(()) => {
                let mut slot = state.tool_install();
                slot.status = "done".to_string();
                slot.tool.clear();
                slot.percent = 100;
                slot.error.clear();
            }
            Err(e) => {
                util::log_line("ERROR", &format!("tool install failed: {e}"));
                let mut slot = state.tool_install();
                slot.status = "error".to_string();
                slot.error = e.clone();
                if slot.tool == "ffmpeg" {
                    slot.ffmpeg_error = e;
                }
            }
        }
    });
}
