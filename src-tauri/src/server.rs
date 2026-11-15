use axum::{
    body::Body,
    extract::{Query, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

use crate::library;
use crate::state::{self, AppState};
use crate::util;
use crate::ytm;

pub type Shared = Arc<AppState>;

const NOT_PORTED: &str = "not yet ported in Riff Rust v0.1";

fn qp(q: &HashMap<String, String>, key: &str) -> String {
    q.get(key).cloned().unwrap_or_default()
}

fn json_ok(payload: Value) -> Response {
    (StatusCode::OK, Json(payload)).into_response()
}

fn json_err(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "success": false, "error": message }))).into_response()
}

fn unported(_: &HashMap<String, String>) -> Response {
    json_ok(json!({ "success": false, "error": NOT_PORTED, "tracks": [] }))
}

fn unported_list() -> Response {
    json_ok(json!({ "success": false, "error": NOT_PORTED, "tracks": [], "results": [] }))
}

/// Shared, pooled HTTP client honouring the configured proxy.
fn http_client(state: &AppState) -> reqwest::Client {
    state.http()
}

/// Spawn a blocking process with a hard timeout; kills it if it overruns.
/// The old version abandoned the child on timeout, leaving yt-dlp processes
/// burning CPU/RAM in the background.
fn run_yt_dlp(state: &AppState, args: &[String], timeout_secs: u64) -> Result<String, String> {
    let bin = util::resolve_binary("yt-dlp").ok_or_else(|| "yt-dlp not found".to_string())?;
    let mut full_args: Vec<String> = Vec::new();
    if let Some(ffmpeg) = util::resolve_binary("ffmpeg") {
        full_args.push("--ffmpeg-location".into());
        full_args.push(
            ffmpeg
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
    }
    full_args.extend(args.iter().cloned());
    if let Ok(proxy) = state.proxy_url.lock() {
        if !proxy.trim().is_empty() {
            full_args.push("--proxy".into());
            full_args.push(proxy.trim().to_string());
        }
    }

    let mut cmd = std::process::Command::new(&bin);
    cmd.args(&full_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    util::hide_console(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start yt-dlp: {e}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "yt-dlp stdout unavailable".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "yt-dlp stderr unavailable".to_string())?;
    let stdout_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        buffer
    });

    let started = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return Err("yt-dlp timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(format!("yt-dlp wait failed: {e}"));
            }
        }
    };
    let out = stdout_thread.join().unwrap_or_default();
    let err = stderr_thread.join().unwrap_or_default();
    if status.success() {
        Ok(String::from_utf8_lossy(&out).to_string())
    } else {
        Err(String::from_utf8_lossy(&err).to_string())
    }
}

// ---------------------------------------------------------------- search

/// yt-dlp `--dump-json` flat playlist search, shared by the soundcloud path and
/// the unified hybrid ("rifflibrary") search pool.
async fn ytdlp_search(state: &Shared, target: &str, platform_name: &str) -> Result<Vec<Value>, String> {
    let args = vec![
        "--dump-json".to_string(),
        "--flat-playlist".to_string(),
        "--no-warnings".to_string(),
        "--no-playlist".to_string(),
        target.to_string(),
    ];
    let state_for_task = state.clone();
    // SoundCloud responds in ~20-25 s on slower networks; a 25 s timeout used to
    // kill it and silently yield an empty half of the pool. Give it room.
    let out = match tokio::task::spawn_blocking(move || run_yt_dlp(&state_for_task, &args, 90)).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("search task failed".to_string()),
    };
    let is_soundcloud = platform_name.eq_ignore_ascii_case("soundcloud");
    let mut tracks = Vec::new();
    for line in out.lines() {
        if let Ok(item) = serde_json::from_str::<Value>(line) {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if id.is_empty() {
                continue;
            }
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
            let artist = item
                .get("uploader")
                .or_else(|| item.get("channel"))
                .or_else(|| item.get("artist"))
                .or_else(|| item.get("uploader_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown artist")
                .to_string();
            let duration = item.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0) as u64;
            let thumbnail = item
                .get("thumbnails")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.last().cloned())
                .and_then(|t| t.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
                .or_else(|| item.get("thumbnail").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .unwrap_or_default();
            let url = item
                .get("webpage_url")
                .or_else(|| item.get("url"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={id}"));
            tracks.push(json!({
                "id": id,
                "title": title,
                "artist": artist,
                "duration": duration,
                "thumbnail": thumbnail,
                "url": url,
                "platform": if is_soundcloud { "soundcloud" } else { "ytmusic" },
                "source": "youtube",
            }));
        }
    }
    Ok(tracks)
}

/// MP3/WAV/FLAC decode everywhere on this machine; m4a/aac/webm/ogg do not.
fn playable_ext(ext: &str) -> bool {
    matches!(ext.to_ascii_lowercase().as_str(), "mp3" | "wav" | "flac")
}

/// Cache file ids go into file paths; keep them to a conservative charset.
fn is_valid_cache_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 16
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn cached_mp3_path(resolved_id: &str) -> PathBuf {
    util::cache_dir().join(format!("{resolved_id}.mp3"))
}

/// Unique temp path inside the cache dir (pid + a random suffix), safe for any
/// number of concurrent transcode/download workers on the same track id.
fn unique_tmp(resolved_id: &str, tag: &str) -> PathBuf {
    util::cache_dir().join(format!(
        "{resolved_id}.{tag}-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ))
}

/// Write `bytes` to cache atomically (tmp + rename). Best effort.
fn write_cache_file(tmp: &std::path::Path, target: &std::path::Path) -> bool {
    if fs::rename(tmp, target).is_ok() {
        return true;
    }
    let ok = fs::copy(tmp, target).is_ok();
    let _ = fs::remove_file(tmp);
    ok
}

/// One download attempt for `url` into `target` (reqwest, rustls).
fn download_once(url: &str, target: &std::path::Path) -> Result<u64, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("client build failed: {e}"))?;
    let mut response = client
        .get(url)
        .send()
        .map_err(|e| format!("request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("status {}", response.status().as_u16()));
    }
    if let Some(expected) = response.content_length() {
        if expected > 96 * 1024 * 1024 {
            return Err(format!("stream too large ({expected} bytes)"));
        }
    }
    let mut file = fs::File::create(target).map_err(|e| format!("create failed: {e}"))?;
    std::io::copy(&mut response, &mut file).map_err(|e| format!("copy failed: {e}"))
}

/// Stream-download `url` into `target`, retrying a few times: googlevideo edges
/// occasionally drop a connection mid-body. ffmpeg is never allowed to touch the
/// network (it 403s), so this is the only way audio leaves the CDN.
fn download_to_file(url: &str, target: &std::path::Path) -> bool {
    for attempt in 1..=3u8 {
        match download_once(url, target) {
            Ok(written) if written > 0 => return true,
            Ok(_) => util::log_line("WARN", &format!("download attempt {attempt}: empty body")),
            Err(e) => util::log_line("WARN", &format!("download attempt {attempt}: {e}")),
        }
        let _ = fs::remove_file(target);
    }
    false
}

/// Fallback when reqwest cannot finish: download the watch page with yt-dlp itself
/// and hand back the resulting file. Slower but extremely reliable here.
fn ytdlp_download(resolved_id: &str) -> Option<PathBuf> {
    if resolved_id.len() != 11
        || !resolved_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let bin = util::resolve_binary("yt-dlp")?;
    let prefix = unique_tmp(resolved_id, "ytdl");
    let template = format!("{}.%(ext)s", prefix.to_string_lossy());
    let args = vec![
        "-q".to_string(),
        "--no-warnings".to_string(),
        "--no-playlist".to_string(),
        "-f".to_string(),
        "bestaudio[ext=m4a]/bestaudio[ext=webm]/bestaudio".to_string(),
        "--no-part".to_string(),
        "-o".to_string(),
        template.clone(),
        format!("https://www.youtube.com/watch?v={resolved_id}"),
    ];
    let mut cmd = std::process::Command::new(bin);
    cmd.args(&args);
    util::hide_console(&mut cmd);
    let status = cmd.status().ok()?;
    if !status.success() {
        util::log_line("WARN", &format!("yt-dlp fallback download failed for {resolved_id}"));
        return None;
    }
    let prefix_str = prefix.to_string_lossy().to_string();
    let entries = fs::read_dir(util::cache_dir()).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.to_string_lossy().starts_with(&prefix_str) && path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Transcode any source (local file or downloaded temp copy) to MP3 via ffmpeg.
/// Returns true on success.
fn ffmpeg_transcode_to_mp3(ffmpeg: &std::path::Path, input: &str, tmp: &std::path::Path) -> bool {
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.arg("-hide_banner")
        .arg("-nostdin")
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-vn")
        .arg("-c:a")
        .arg("libmp3lame")
        .arg("-q:a")
        .arg("4")
        .arg("-f")
        .arg("mp3")
        .arg(tmp);
    util::hide_console(&mut cmd);
    match cmd.output() {
        Ok(out) => {
            if out.status.success() {
                true
            } else {
                let tail: String = String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or("")
                    .chars()
                    .take(200)
                    .collect();
                util::log_line("WARN", &format!("ffmpeg transcode failed: {tail}"));
                false
            }
        }
        Err(e) => {
            util::log_line("WARN", &format!("ffmpeg spawn failed: {e}"));
            false
        }
    }
}

/// Background: pull a resolved stream into `tracks/<id>.mp3` so repeat plays are
/// local and decode on machines without AAC/Opus support (this build of WebView2
/// included). Progressive MP3 CDN links are stored byte-for-byte; everything else
/// is decoded/re-encoded by ffmpeg. Best effort: failure leaves the file missing
/// and the next stream-info call simply retries.
fn cache_resolved_stream(resolved_id: &str, _ext_hint: &str, url: &str) {
    if !is_valid_cache_id(resolved_id) {
        return;
    }
    let target = cached_mp3_path(resolved_id);
    if target.is_file() {
        return;
    }
    let _ = fs::create_dir_all(util::cache_dir());
    let tmp = unique_tmp(resolved_id, "mp3.part");
    let started = Instant::now();
    let url_path = url.split('?').next().unwrap_or(url);
    let looks_like_mp3 = url_path.to_ascii_lowercase().ends_with(".mp3");
    util::log_line("INFO", &format!("background mp3 cache: {resolved_id} (mp3={looks_like_mp3})"));
    if looks_like_mp3 {
        if !download_to_file(url, &tmp) {
            let _ = fs::remove_file(&tmp);
            util::log_line("WARN", &format!("background mp3 cache: download failed for {resolved_id}"));
            return;
        }
    } else {
        let Some(ffmpeg) = util::resolve_binary("ffmpeg") else {
            util::log_line("WARN", "ffmpeg missing; skipping background mp3 cache");
            return;
        };
        let src = unique_tmp(resolved_id, "download");
        if !download_to_file(url, &src) {
            let _ = fs::remove_file(&src);
            util::log_line("WARN", &format!("background mp3 cache: download failed for {resolved_id}"));
            return;
        }
        let ok = ffmpeg_transcode_to_mp3(&ffmpeg, &src.to_string_lossy(), &tmp);
        let _ = fs::remove_file(&src);
        if !ok {
            let _ = fs::remove_file(&tmp);
            util::log_line("WARN", &format!("background mp3 cache: transcode failed for {resolved_id}"));
            return;
        }
    }
    if write_cache_file(&tmp, &target) {
        util::log_line(
            "INFO",
            &format!("cached stream {resolved_id} -> {} ({} ms)", target.display(), started.elapsed().as_millis()),
        );
    } else {
        let _ = fs::remove_file(&tmp);
    }
}

/// Synchronously transcode a remote URL (or local file) into `tracks/<id>.mp3`.
/// Used when the player reports a decode failure (MEDIA_ERR_SRC_NOT_SUPPORTED):
/// the renderer retries stream-info with `forceYtDlp=1` and gets a guaranteed
/// playable local file instead of another unplayable AAC/Opus stream.
fn transcode_to_cache_sync(resolved_id: &str, input: &str) -> Option<PathBuf> {
    if !is_valid_cache_id(resolved_id) {
        return None;
    }
    let target = cached_mp3_path(resolved_id);
    if target.is_file() {
        return Some(target);
    }
    let _ = fs::create_dir_all(util::cache_dir());
    let tmp = unique_tmp(resolved_id, "mp3.part");
    let Some(ffmpeg) = util::resolve_binary("ffmpeg") else {
        util::log_line("WARN", "ffmpeg missing; cannot transcode to mp3");
        return None;
    };
    let started = Instant::now();
    let is_url = input.starts_with("http://") || input.starts_with("https://");
    let src_owned;
    let mut downloaded_by_ytdlp: Option<PathBuf> = None;
    let src = if is_url {
        let src_path = unique_tmp(resolved_id, "download");
        if !download_to_file(input, &src_path) {
            let _ = fs::remove_file(&src_path);
            // Last-resort fallback for flaky googlevideo edges: let yt-dlp grab the file.
            if let Some(fallback) = ytdlp_download(resolved_id) {
                src_owned = fallback.to_string_lossy().to_string();
                downloaded_by_ytdlp = Some(fallback);
                src_owned.as_str()
            } else {
                util::log_line("WARN", &format!("transcode failed (download) for {resolved_id}"));
                return None;
            }
        } else {
            src_owned = src_path.to_string_lossy().to_string();
            src_owned.as_str()
        }
    } else {
        input
    };
    let ok = ffmpeg_transcode_to_mp3(&ffmpeg, src, &tmp);
    if is_url {
        let _ = fs::remove_file(src);
    }
    if let Some(fallback) = downloaded_by_ytdlp {
        let _ = fs::remove_file(&fallback);
    }
    if !ok {
        let _ = fs::remove_file(&tmp);
        util::log_line("WARN", &format!("transcode failed for {resolved_id}"));
        return None;
    }
    if write_cache_file(&tmp, &target) {
        util::log_line(
            "INFO",
            &format!("transcoded {resolved_id} -> {} ({} ms)", target.display(), started.elapsed().as_millis()),
        );
        Some(target)
    } else {
        let _ = fs::remove_file(&tmp);
        None
    }
}

async fn search(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let query = qp(&q, "q");
    let platform = qp(&q, "platform");
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(10).clamp(1, 24);
    if query.trim().is_empty() {
        return json_ok(json!({ "success": true, "tracks": [] }));
    }

    // Hybrid unified search (upstream's "Riff Library hybrid streaming"):
    // YouTube Music + SoundCloud interleaved 2:1, single ranked list.
    if platform.eq_ignore_ascii_case("rifflibrary") {
        let cache_key = format!("rifflib:{}:{}", limit, query.trim().to_lowercase());
        if let Some(cached) =
            state::cache_get(&state.search_cache, &cache_key, Duration::from_secs(300))
        {
            return json_ok(json!({ "success": true, "tracks": cached }));
        }
        let sc_search_target = format!("scsearch{}:{}", limit, query.trim());
        let soundcloud_search = crate::soundcloud::search_tracks(&state, query.trim(), limit);
        let soundcloud_fallback = ytdlp_search(&state, &sc_search_target, "soundcloud");
        let sc_tracks_future = async move {
            match soundcloud_search.await {
                Ok(tracks) => Ok(tracks),
                Err(e) => {
                    util::log_line("WARN", &format!("{e}; falling back to yt-dlp soundcloud search"));
                    match tokio::time::timeout(Duration::from_secs(25), soundcloud_fallback).await {
                        Ok(result) => result,
                        Err(_) => Err("soundcloud search timed out".to_string()),
                    }
                }
            }
        };
        let (ytm_tracks, sc_tracks) = futures_util::future::join(
            ytm::search(&state.http(), query.trim(), limit),
            sc_tracks_future,
        )
        .await;
        let ytm_tracks = ytm_tracks.unwrap_or_default();
        let sc_tracks = sc_tracks.unwrap_or_default();
        let mut combined: Vec<Value> = Vec::with_capacity(ytm_tracks.len() + sc_tracks.len());
        let mut yi = 0usize;
        let mut si = 0usize;
        while yi < ytm_tracks.len() || si < sc_tracks.len() {
            if yi < ytm_tracks.len() {
                combined.push(ytm_tracks[yi].clone());
                yi += 1;
            }
            if yi < ytm_tracks.len() {
                combined.push(ytm_tracks[yi].clone());
                yi += 1;
            }
            if si < sc_tracks.len() {
                combined.push(sc_tracks[si].clone());
                si += 1;
            }
        }
        for t in combined.iter_mut() {
            if let Some(map) = t.as_object_mut() {
                if let Some(id) = map.get("id").and_then(|v| v.as_str()).map(String::from) {
                    let cached = library::find_track_by_id(&id).is_some();
                    map.insert("isCached".to_string(), json!(cached));
                }
            }
        }
        state::cache_put(&state.search_cache, cache_key, combined.clone(), 128, Duration::from_secs(300));
        return json_ok(json!({ "success": true, "tracks": combined }));
    }

    // Fast path: direct SoundCloud search. No child process, ~1 s.
    if platform.eq_ignore_ascii_case("soundcloud") {
        let cache_key = format!("sc:{}:{}", limit, query.trim().to_lowercase());
        if let Some(cached) =
            state::cache_get(&state.search_cache, &cache_key, Duration::from_secs(300))
        {
            return json_ok(json!({ "success": true, "tracks": cached }));
        }
        match crate::soundcloud::search_tracks(&state, query.trim(), limit).await {
            Ok(tracks) if !tracks.is_empty() => {
                let mut tracks = tracks;
                for t in tracks.iter_mut() {
                    if let Some(map) = t.as_object_mut() {
                        if let Some(id) = map.get("id").and_then(|v| v.as_str()).map(String::from) {
                            let cached = library::find_track_by_id(&id).is_some();
                            map.insert("isCached".to_string(), json!(cached));
                        }
                    }
                }
                state::cache_put(
                    &state.search_cache,
                    cache_key,
                    tracks.clone(),
                    128,
                    Duration::from_secs(300),
                );
                return json_ok(json!({ "success": true, "tracks": tracks }));
            }
            Ok(_) => util::log_line("INFO", "soundcloud search returned no tracks; using yt-dlp"),
            Err(e) => util::log_line("WARN", &format!("{e}; using yt-dlp")),
        }
    }

    // Fast path: direct YouTube Music search. No child process, no Python startup.
    if !platform.eq_ignore_ascii_case("soundcloud") {
        let cache_key = format!("ytm:{}:{}", limit, query.trim().to_lowercase());
        if let Some(cached) =
            state::cache_get(&state.search_cache, &cache_key, Duration::from_secs(300))
        {
            return json_ok(json!({ "success": true, "tracks": cached }));
        }
        match ytm::search(&state.http(), query.trim(), limit).await {
            Ok(tracks) if !tracks.is_empty() => {
                state::cache_put(
                    &state.search_cache,
                    cache_key,
                    tracks.clone(),
                    128,
                    Duration::from_secs(300),
                );
                return json_ok(json!({ "success": true, "tracks": tracks }));
            }
            Ok(_) => util::log_line("INFO", "ytmusic search returned no tracks; using yt-dlp"),
            Err(e) => util::log_line("WARN", &format!("{e}; using yt-dlp")),
        }
    }

    let prefix = if platform.eq_ignore_ascii_case("soundcloud") { "scsearch" } else { "ytsearch" };
    let search_target = format!("{prefix}{limit}:{query}");
    match ytdlp_search(&state, &search_target, &platform).await {
        Ok(tracks) => json_ok(json!({ "success": true, "tracks": tracks })),
        Err(e) => {
            util::log_line("WARN", &format!("search failed: {e}"));
            json_ok(json!({ "success": false, "tracks": [], "error": e }))
        }
    }
}

// ------------------------------------------------------------- stream info

async fn stream_info(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let id = qp(&q, "id");
    let url = qp(&q, "url");
    let title = qp(&q, "title");
    let artist = qp(&q, "artist");
    let duration_q = qp(&q, "duration");
    let quality = qp(&q, "quality");
    let platform = qp(&q, "platform").to_ascii_lowercase();
    // Set by the renderer on its second retry after a decode failure; it means
    // the direct CDN stream would not play, so hand back a local MP3 instead.
    let force_yt_dlp = q.get("forceYtDlp").map(|v| v == "1").unwrap_or(false);

    // Local import (library/ directory)? Bare video-id cache hits are NOT handled
    // here: they flow into the playable-cache and transcode lanes below, which know
    // the difference between decodeable (mp3/wav/flac) and code=4 files (m4a/webm).
    if id.starts_with("local_") || id.starts_with("riff-") {
        if let Some(path) = library::find_track_by_id(&id) {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_else(|| "mp3".into());
            let local_payload = |mime: &str| {
                let port = state.server_port;
                json_ok(json!({
                    "success": true,
                    "isLocal": true,
                    "mimeType": mime,
                    "streamUrl": format!("http://127.0.0.1:{port}/api/local-track?id={id}"),
                    "resolvedId": id,
                    "title": title,
                    "artist": artist,
                    "duration": duration_q.parse::<u64>().unwrap_or(0),
                }))
            };
            // Playable as-is (mp3/wav/flac)? serve directly.
            if playable_ext(&ext) {
                return local_payload(util::mime_from_ext(&ext));
            }
            // m4a/webm/ogg on a webview that cannot decode them: transcode once
            // into the tracks cache; the original library file is left untouched.
            let mp3_id = id.clone();
            let mp3_src = path.to_string_lossy().to_string();
            let res = tokio::task::spawn_blocking(move || {
                transcode_to_cache_sync(&mp3_id, &mp3_src)
            })
            .await;
            if let Ok(Some(_)) = res {
                return local_payload("audio/mpeg");
            }
            // Machines with working codecs: hand back the original file anyway.
            return local_payload(util::mime_from_ext(&ext));
        }
    }

    let target = if !url.trim().is_empty() {
        url.clone()
    } else if !id.is_empty() {
        format!("https://www.youtube.com/watch?v={id}")
    } else {
        return json_err(StatusCode::BAD_REQUEST, "missing url or id");
    };

    // Downloaded cache hit? Serve the local file; instant and works offline.
    if let Some(path) = library::find_playable_cached_by_id(&id) {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_else(|| "mp3".into());
        let mime = util::mime_from_ext(&ext);
        let port = state.server_port;
        return json_ok(json!({
            "success": true,
            "isLocal": true,
            "mimeType": mime,
            "streamUrl": format!("http://127.0.0.1:{port}/api/local-track?id={id}"),
            "resolvedId": id,
            "title": title,
            "artist": artist,
            "duration": duration_q.parse::<u64>().unwrap_or(0),
        }));
    }

    // Only unplayable cache files (m4a/webm/aac)? Transcode once to MP3 so the
    // WebView2 Media Foundation decoder (mp3/wav/flac only on this machine) gets a
    // stream it can actually render; then drop the unplayable source (disk hygiene).
    if let Some(path) = library::find_any_cached_by_id(&id) {
        let input_id = id.clone();
        let input = path.to_string_lossy().to_string();
        let res = tokio::task::spawn_blocking(move || {
            transcode_to_cache_sync(&input_id, &input)
        })
        .await;
        if let Ok(Some(_)) = res {
            let _ = fs::remove_file(&path);
            let port = state.server_port;
            return json_ok(json!({
                "success": true,
                "isLocal": true,
                "mimeType": "audio/mpeg",
                "streamUrl": format!("http://127.0.0.1:{port}/api/local-track?id={id}"),
                "resolvedId": id,
                "title": title,
                "artist": artist,
                "duration": duration_q.parse::<u64>().unwrap_or(0),
            }));
        }
        util::log_line("WARN", &format!("could not transcode cached stream {id}; resolving fresh"));
    }

    let is_soundcloud = platform == "soundcloud" || url.to_ascii_lowercase().contains("soundcloud.com");

    // SoundCloud direct path: resolve the permalink to a progressive MP3 in about
    // a second over SoundCloud's JSON API instead of spawning yt-dlp (about 10 to
    // 25 seconds). Falls back to yt-dlp below when the direct resolve fails.
    if is_soundcloud {
        match crate::soundcloud::resolve_progressive_mp3(&state, &url).await {
            Some(mp3_url) => {
                let port = state.server_port;
                let cache_id = if id.len() <= 16
                    && id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                {
                    id.clone()
                } else {
                    format!("sc{}", &util::sha1_hex(url.as_bytes())[..11])
                };
                let proxied = format!(
                    "http://127.0.0.1:{port}/api/audio-proxy?url={}",
                    util::percent_encode(&mp3_url)
                );
                {
                    let dl_url = mp3_url.clone();
                    let dl_id = cache_id.clone();
                    std::thread::spawn(move || cache_resolved_stream(&dl_id, "mp3", &dl_url));
                }
                let payload = json!({
                    "success": true,
                    "isLocal": false,
                    "streamUrl": proxied,
                    "rawStreamUrl": mp3_url,
                    "proxyUrl": proxied,
                    "isHls": false,
                    "binaryNote": Value::Null,
                    "mimeType": "audio/mpeg",
                    "resolvedId": cache_id,
                    "duration": duration_q.parse::<u64>().unwrap_or(0),
                    "title": title,
                    "artist": artist,
                    "thumbnail": "",
                    "direct": true,
                });
                state::cache_put(
                    &state.stream_cache,
                    format!("stream:{}:{}:{}", qp(&q, "quality"), id, url),
                    payload.clone(),
                    256,
                    Duration::from_secs(600),
                );
                return json_ok(payload);
            }
            None => {
                util::log_line("INFO", "soundcloud: direct resolve failed; falling back to yt-dlp");
            }
        }
    }

    // Repeated plays of the same track used to spawn yt-dlp every time. Cache the
    // resolved URL briefly; YouTube signatures/URLs are short-lived, so keep the TTL tight.
    // `bust` (and the renderer's decode-failure retry) bypasses the cache entirely.
    let no_cache = force_yt_dlp || q.contains_key("bust");
    let cache_key = format!("stream:{quality}:{id}:{target}");
    if !no_cache {
        if let Some(cached) =
            state::cache_get(&state.stream_cache, &cache_key, Duration::from_secs(600))
        {
            return json_ok(cached);
        }
    }

    // Forced re-encode lane (renderer's post-failure retry, forceYtDlp=1): resolve
    // with the inner API and transcode straight to a local MP3. Skips the whole
    // yt-dlp Python spawn (~8-10 s) that the fallback below would otherwise pay.
    if force_yt_dlp && !is_soundcloud {
        if let Some(direct_url) = crate::ytm_player::direct_audio_url(&id, &quality, &state).await {
            let input = direct_url.clone();
            let tid = id.clone();
            let res = tokio::task::spawn_blocking(move || {
                transcode_to_cache_sync(&tid, &input)
            })
            .await;
            if let Ok(Some(_)) = res {
                let port = state.server_port;
                return json_ok(json!({
                    "success": true,
                    "isLocal": true,
                    "mimeType": "audio/mpeg",
                    "streamUrl": format!("http://127.0.0.1:{port}/api/local-track?id={id}"),
                    "resolvedId": id,
                    "title": title,
                    "artist": artist,
                    "duration": duration_q.parse::<u64>().unwrap_or(0),
                }));
            }
        }
        // inner API or transcode failed: fall back to the yt-dlp path with a local transcode after resolve
    }

    // Fast path: inner youtubei `/v1/player` — no subprocess, ~0.2-0.5s.
    // Skipped when the renderer asked to force yt-dlp after a decode failure or
    // for SoundCloud links (SoundCloud uses its own direct resolver above).
    if !force_yt_dlp && !is_soundcloud {
        if let Some(direct_url) = crate::ytm_player::direct_audio_url(&id, &quality, &state).await {
            let resolved_id = id.clone();
        let port = state.server_port;
        let proxied = format!(
            "http://127.0.0.1:{port}/api/audio-proxy?url={}",
            util::percent_encode(&direct_url)
        );
        util::log_line("INFO", &format!("stream-info(direct) resolved {resolved_id}"));
        let payload = json!({
            "success": true,
            "isLocal": false,
            "streamUrl": proxied,
            "rawStreamUrl": direct_url,
            "proxyUrl": proxied,
            "isHls": direct_url.contains(".m3u8"),
            "binaryNote": Value::Null,
            "mimeType": "audio/mp4",
            "resolvedId": resolved_id,
            "duration": duration_q.parse::<u64>().unwrap_or(0),
            "title": title,
            "artist": artist,
            "thumbnail": "",
            "direct": true,
        });
        {
            let dl_url = direct_url.clone();
            let dl_id = resolved_id.clone();
            std::thread::spawn(move || cache_resolved_stream(&dl_id, "m4a", &dl_url));
        }
        state::cache_put(&state.stream_cache, cache_key, payload.clone(), 256, Duration::from_secs(600));
        return json_ok(payload);
        }
    }
    let format_arg = if is_soundcloud {
        // SoundCloud serves progressive MP3; grabbing it skips the AAC/Opus decode gap
        "http_mp3_128/bestaudio[ext=mp3][protocol^=http]/bestaudio[ext=mp3]/bestaudio[protocol^=http]/bestaudio".to_string()
    } else {
        match quality.as_str() {
            "low" | "worst" => "worstaudio[ext=m4a]/worstaudio".to_string(),
            "medium" => "bestaudio[abr<=96][ext=m4a]/bestaudio[ext=m4a]/bestaudio[abr<=96]/bestaudio".to_string(),
            _ => "bestaudio[ext=m4a]/bestaudio[ext=mp4]/bestaudio".to_string(),
        }
    };

    let args = vec![
        "-j".to_string(),
        "--no-warnings".to_string(),
        "--no-playlist".to_string(),
        "--socket-timeout".to_string(),
        "8".to_string(),
        "--retries".to_string(),
        "1".to_string(),
        "--format".to_string(),
        format_arg,
        target.clone(),
    ];
    let state_for_task = state.clone();
    let target_for_log = target.clone();
    let out = match tokio::task::spawn_blocking(move || run_yt_dlp(&state_for_task, &args, 45)).await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            util::log_line("WARN", &format!("stream-info failed for {target_for_log}: {e}"));
            return json_ok(json!({ "success": false, "error": e }));
        }
        Err(e) => {
            util::log_line("WARN", &format!("stream-info task failed for {target_for_log}: {e}"));
            return json_ok(json!({ "success": false, "error": "stream-info task failed" }));
        }
    };
    let info: Value = match serde_json::from_str(&out) {
        Ok(v) => v,
        Err(e) => return json_ok(json!({ "success": false, "error": format!("yt-dlp output parse error: {e}") })),
    };
    // Prefer an audio-only adaptive format when yt-dlp splits them
    let stream_url = info
        .get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            info.get("requested_formats")
                .and_then(|v| v.as_array())
                .and_then(|arr| {
                    arr.iter()
                        .find(|f| f.get("acodec").and_then(|a| a.as_str()).map(|a| a != "none").unwrap_or(false))
                        .or_else(|| arr.first())
                })
                .and_then(|f| f.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
        })
        .unwrap_or_default();
    if stream_url.is_empty() {
        return json_ok(json!({ "success": false, "error": "no stream url in yt-dlp output" }));
    }
    let resolved_id = info.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let resolved_id = if resolved_id.len() == 11 { resolved_id } else { id.clone() };
    let duration = info.get("duration").and_then(|v| v.as_f64()).unwrap_or(duration_q.parse::<f64>().unwrap_or(0.0)) as u64;
    let thumbnail = info
        .get("thumbnail")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| item_thumbnail(&info))
        .unwrap_or_default();
    let ext = info.get("ext").and_then(|v| v.as_str()).unwrap_or("m4a").to_string();
    let mime = util::mime_from_ext(&ext);
    // Match upstream's shape: the renderer picks `proxyUrl || streamUrl` for
    // non-local tracks. The raw CDN URL breaks the Web Audio pipeline because
    // googlevideo.com does not send CORS headers; the loopback proxy always works.
    // When the renderer flagged a decode failure (forceYtDlp=1) the proxy URL would
    // just fail again, so re-encode the resolved stream into a local MP3 and hand
    // back a guaranteed-playable file instead.
    if force_yt_dlp {
        let input = stream_url.clone();
        let tid = resolved_id.clone();
        let res = tokio::task::spawn_blocking(move || {
            transcode_to_cache_sync(&tid, &input)
        })
        .await;
        if let Ok(Some(_)) = res {
            let port2 = state.server_port;
            return json_ok(json!({
                "success": true,
                "isLocal": true,
                "mimeType": "audio/mpeg",
                "streamUrl": format!("http://127.0.0.1:{port2}/api/local-track?id={resolved_id}"),
                "resolvedId": resolved_id,
                "duration": duration,
                "title": info.get("title").and_then(|v| v.as_str()).unwrap_or(&title),
                "artist": info.get("uploader").and_then(|v| v.as_str()).unwrap_or(&artist),
                "thumbnail": thumbnail.clone(),
                "direct": true,
            }));
        }
        util::log_line("WARN", "forceYtDlp transcode failed; falling back to proxied stream");
    }
    let port = state.server_port;
    let proxied = format!(
        "http://127.0.0.1:{port}/api/audio-proxy?url={}",
        util::percent_encode(&stream_url)
    );
    let payload = json!({
        "success": true,
        "isLocal": false,
        "streamUrl": proxied,
        "rawStreamUrl": stream_url,
        "proxyUrl": proxied,
        "isHls": stream_url.contains(".m3u8"),
        "binaryNote": Value::Null,
        "mimeType": mime,
        "resolvedId": resolved_id,
        "duration": duration,
        "title": info.get("title").and_then(|v| v.as_str()).unwrap_or(&title),
        "artist": info.get("uploader").and_then(|v| v.as_str()).unwrap_or(&artist),
        "thumbnail": thumbnail,
        "direct": true,
    });
    // Background cache so the next play is offline-grade fast (upstream parity).
    // Anything that is not MP3 is re-encoded during the copy; the tracks folder
    // only ever holds MP3 and this keeps stale m4a/webm caches from piling up.
    {
        let dl_url = stream_url.clone();
        let dl_id = resolved_id.clone();
        let dl_ext = ext.clone();
        std::thread::spawn(move || cache_resolved_stream(&dl_id, &dl_ext, &dl_url));
    }
    state::cache_put(
        &state.stream_cache,
        cache_key,
        payload.clone(),
        256,
        Duration::from_secs(600),
    );
    json_ok(payload)
}

fn item_thumbnail(item: &Value) -> Option<String> {
    item.get("thumbnails")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.last().cloned())
        .and_then(|t| t.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
}

// ---------------------------------------------------- local file streaming

fn parse_range_header(raw: Option<&str>, total: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some(spec) = raw.strip_prefix("bytes=") else {
        return Err(());
    };
    // Multi-range requests are legal but browsers only need the first range for audio.
    let spec = spec.split(',').next().unwrap_or("").trim();
    let (start_text, end_text) = spec.split_once('-').ok_or(())?;
    if total == 0 {
        return Err(());
    }
    if start_text.is_empty() {
        // suffix range: last N bytes
        let suffix: u64 = end_text.parse().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let length = suffix.min(total);
        Ok(Some((total - length, total - 1)))
    } else {
        let start: u64 = start_text.parse().map_err(|_| ())?;
        if start >= total {
            return Err(());
        }
        let end = if end_text.is_empty() {
            total - 1
        } else {
            end_text.parse::<u64>().map_err(|_| ())?.min(total - 1)
        };
        if end < start {
            return Err(());
        }
        Ok(Some((start, end)))
    }
}

fn range_not_satisfiable(total: u64) -> Response {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(axum::http::header::CONTENT_RANGE, format!("bytes */{total}"))
        .header(axum::http::header::ACCEPT_RANGES, "bytes")
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// Stream the requested byte range straight from disk. The previous implementation
/// read the entire track into a Vec for every request, so a 60 MB FLAC cost 60 MB of
/// RAM (plus a second copy while slicing) on each seek.
async fn serve_file_with_range(path: &PathBuf, range_header: Option<&str>) -> Response {
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => return json_err(StatusCode::NOT_FOUND, "file not found"),
    };
    let total = match file.metadata().await {
        Ok(meta) => meta.len(),
        Err(_) => return json_err(StatusCode::INTERNAL_SERVER_ERROR, "could not stat file"),
    };
    let parsed = match parse_range_header(range_header, total) {
        Ok(parsed) => parsed,
        Err(_) => return range_not_satisfiable(total),
    };
    let (start, end, is_range) = match parsed {
        Some((start, end)) => (start, end, true),
        None => (0, total.saturating_sub(1), false),
    };
    let length = if total == 0 {
        0
    } else {
        end.saturating_sub(start) + 1
    };
    if start > 0 && file.seek(SeekFrom::Start(start)).await.is_err() {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, "could not seek file");
    }

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_else(|| "mp3".into());
    let stream = ReaderStream::with_capacity(file.take(length), 64 * 1024);
    let mut builder = Response::builder()
        .status(if is_range {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(axum::http::header::CONTENT_TYPE, util::mime_from_ext(&ext))
        .header(axum::http::header::ACCEPT_RANGES, "bytes")
        .header(axum::http::header::CONTENT_LENGTH, length.to_string());
    if is_range {
        builder = builder.header(
            axum::http::header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    }
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn local_track(Query(q): Query<HashMap<String, String>>, req: Request<Body>) -> Response {
    let range_header = req
        .headers()
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok());
    let id = qp(&q, "id");
    match library::find_track_by_id(&id) {
        Some(path) => serve_file_with_range(&path, range_header).await,
        None => json_err(StatusCode::NOT_FOUND, "track not found"),
    }
}

async fn library_file(Query(q): Query<HashMap<String, String>>, req: Request<Body>) -> Response {
    local_track(Query(q), req).await
}

async fn local_thumbnail(State(_state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let id = qp(&q, "id");
    for ext in ["jpg", "png", "webp"] {
        let p = util::covers_dir().join(format!("{id}.{ext}"));
        if p.is_file() {
            if let Ok(bytes) = fs::read(&p) {
                return Response::builder()
                    .header(axum::http::header::CONTENT_TYPE, util::mime_from_ext(ext))
                    .body(Body::from(bytes))
                    .unwrap();
            }
        }
    }
    json_err(StatusCode::NOT_FOUND, "no thumbnail")
}

// ------------------------------------------------------------- library API

async fn library_list(State(_state): State<Shared>) -> Response {
    let tracks = library::load_library();
    json_ok(json!({ "success": true, "tracks": tracks, "count": tracks.len() }))
}

async fn library_add(State(_state): State<Shared>, Json(body): Json<Value>) -> Response {
    let items = body.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    library::add_to_library(&items);
    json_ok(json!({ "success": true, "added": items.len() }))
}

async fn library_update(State(_state): State<Shared>, Json(body): Json<Value>) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if id.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "missing id");
    }
    if let Some(obj) = body.as_object() {
        let patch: serde_json::Map<String, Value> = obj
            .iter()
            .filter(|(k, _)| k.as_str() != "id")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        library::update_library_entry(&id, &patch);
    }
    json_ok(json!({ "success": true }))
}

async fn library_remove(State(_state): State<Shared>, Json(body): Json<Value>) -> Response {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    if id.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "missing id");
    }
    library::remove_library_entry(&id);
    json_ok(json!({ "success": true }))
}

async fn offline_library(State(_state): State<Shared>) -> Response {
    let tracks = library::load_library();
    let mut total_bytes: u64 = 0;
    let mut out: Vec<Value> = tracks
        .into_iter()
        .map(|mut t| {
            if let Some(id) = t.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()) {
                t["cleanId"] = json!(id);
            }
            t["isLocal"] = json!(true);
            if let Some(size) = t.get("fileSize").and_then(|v| v.as_u64()) {
                total_bytes += size;
            } else if let Some(id) = t.get("id").and_then(|v| v.as_str()) {
                if let Some(p) = library::find_track_by_id(id) {
                    total_bytes += p.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
            t
        })
        .collect();
    // downloaded (yt-dlp) tracks in the cache dir
    if let Ok(entries) = fs::read_dir(util::cache_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let ext = path.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "mp3".into());
            let size = path.metadata().map(|m| m.len()).unwrap_or(0);
            total_bytes += size;
            out.push(json!({
                "id": stem,
                "cleanId": stem,
                "title": stem,
                "artist": "",
                "duration": 0,
                "ext": ext,
                "fileSize": size,
                "isCached": true,
                "isLocal": true,
                "platform": "download",
            }));
        }
    }
    let count = out.len();
    json_ok(json!({ "success": true, "tracks": out, "count": count, "totalBytes": total_bytes }))
}

async fn offline_delete(State(_state): State<Shared>, Query(q): Query<HashMap<String, String>>, req: Request<Body>) -> Response {
    let body_raw = match req.method().as_str() {
        "POST" => {
            let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024).await.unwrap_or_default();
            String::from_utf8_lossy(&bytes).to_string()
        }
        _ => String::new(),
    };
    let body_json: Value = serde_json::from_str(&body_raw).unwrap_or(json!({}));
    let mut id = qp(&q, "id");
    if id.is_empty() {
        id = body_json.get("id").and_then(|v| v.as_str()).or_else(|| body_json.get("cleanId").and_then(|v| v.as_str())).unwrap_or_default().to_string();
    }
    if id.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "missing id");
    }
    // downloaded cache file (match the exact id or `<id>.<ext>`, never a shared prefix)
    let mut removed = false;
    let exact_prefix = format!("{id}.");
    if let Ok(entries) = fs::read_dir(util::cache_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == id || name.starts_with(&exact_prefix) {
                let _ = fs::remove_file(entry.path());
                removed = true;
            }
        }
    }
    // library entry
    if library::remove_library_entry(&id) {
        removed = true;
    }
    json_ok(json!({ "success": true, "removed": removed }))
}

async fn clear_offline_library(State(_state): State<Shared>) -> Response {
    let freed = util::clear_dir_contents(&util::cache_dir());
    json_ok(json!({ "success": true, "freedBytes": freed }))
}

async fn check_cache(State(_state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let id = qp(&q, "id");
    let cached = !id.is_empty() && library::find_cached_by_id(&id).is_some();
    json_ok(json!({ "success": true, "cached": cached, "id": id }))
}

// --------------------------------------------------------------- download

async fn download(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let url = qp(&q, "url");
    let id = qp(&q, "id");
    let title = qp(&q, "title");
    let artist = qp(&q, "artist");
    let format = qp(&q, "format");
    let format = if format.is_empty() { "mp3".to_string() } else { format };
    let custom_dir = qp(&q, "customDir");

    let target = if !url.trim().is_empty() {
        url.clone()
    } else if !id.is_empty() {
        format!("https://www.youtube.com/watch?v={id}")
    } else {
        return json_err(StatusCode::BAD_REQUEST, "missing url or id");
    };

    let dest_dir = if !custom_dir.trim().is_empty() {
        PathBuf::from(custom_dir.trim())
    } else if !state.download_dir.lock().map(|d| d.clone()).unwrap_or_default().trim().is_empty() {
        PathBuf::from(state.download_dir.lock().unwrap().clone())
    } else {
        #[cfg(target_os = "windows")]
        {
            std::env::var("USERPROFILE").map(|p| PathBuf::from(p).join("Downloads")).unwrap_or_else(|_| PathBuf::from("Downloads"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            std::env::var("HOME").map(|p| PathBuf::from(p).join("Downloads")).unwrap_or_else(|_| PathBuf::from("Downloads"))
        }
    };
    let _ = fs::create_dir_all(&dest_dir);

    // Output stem: prefer the stable id, fall back to "artist - title"
    let clean_id = if !id.trim().is_empty() {
        id.trim().chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect::<String>()
    } else {
        let stem = format!("{artist} - {title}").trim().replace(['\\', '/', ':', '*', '?', '"', '<', '>', '|'], "_");
        if stem.trim().is_empty() { "track".to_string() } else { stem.trim().to_string() }
    };
    let out_template = dest_dir.join(format!("{clean_id}.%(ext)s")).to_string_lossy().to_string();

    let audio_bitrate = match qp(&q, "quality").as_str() {
        "low" => "128K",
        "medium" => "192K",
        _ => "0", // best
    };
    let audio_format = match format.to_ascii_lowercase().as_str() {
        "flac" | "opus" | "wav" | "mp3" | "m4a" | "aac" | "vorbis" => format.to_ascii_lowercase(),
        _ => "mp3".to_string(),
    };

    let args = vec![
        "--no-warnings".to_string(),
        "--no-playlist".to_string(),
        "-x".to_string(),
        "--audio-format".to_string(),
        audio_format.clone(),
        "--audio-quality".to_string(),
        audio_bitrate.to_string(),
        "--embed-thumbnail".to_string(),
        "--add-metadata".to_string(),
        "-o".to_string(),
        out_template,
        target.clone(),
    ];

    // Blocking spawn on a blocking thread (frontend awaits the whole download)
    let result = tokio::task::spawn_blocking(move || run_yt_dlp(&state, &args, 900)).await;
    match result {
        Ok(Ok(_)) => {
            // Find the produced file to report its path
            let mut file_path = dest_dir.join(format!("{clean_id}.{audio_format}"));
            if !file_path.exists() {
                for ext in ["mp3", "flac", "opus", "wav", "m4a", "aac", "ogg", "webm"] {
                    let candidate = dest_dir.join(format!("{clean_id}.{ext}"));
                    if candidate.exists() {
                        file_path = candidate;
                        break;
                    }
                }
            }
            json_ok(json!({
                "success": true,
                "directory": dest_dir.to_string_lossy(),
                "file": file_path.to_string_lossy(),
                "format": audio_format,
            }))
        }
        Ok(Err(e)) => json_ok(json!({ "success": false, "error": e })),
        Err(e) => json_ok(json!({ "success": false, "error": format!("download task failed: {e}") })),
    }
}

// --------------------------------------------------------------- discovery

async fn vibe(State(state): State<Shared>, Json(body): Json<Value>) -> Response {
    match tokio::time::timeout(
        std::time::Duration::from_secs(7),
        crate::discovery::vibe(&state, &body),
    )
    .await
    {
        Ok(payload) => json_ok(payload),
        Err(_) => json_ok(json!({ "success": false, "tracks": [], "error": "timeout" })),
    }
}

async fn hot(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    json_ok(crate::discovery::hot(&state, &q).await)
}

async fn trending(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    json_ok(crate::discovery::trending(&state, &q).await)
}

async fn top_chart(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    json_ok(crate::discovery::top_chart(&state, &q).await)
}

async fn artist(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    json_ok(crate::discovery::artist(&state, &q).await)
}

// ------------------------------------------------------------------- tools

async fn tools_status(State(state): State<Shared>) -> Response {
    let ytdlp = util::resolve_binary("yt-dlp");
    let ffmpeg = util::resolve_binary("ffmpeg");
    #[cfg(target_os = "macos")]
    let ffmpeg_optional = true;
    #[cfg(not(target_os = "macos"))]
    let ffmpeg_optional = false;

    let progress = state.tool_install().clone();
    let step = if progress.status == "downloading" {
        format!("downloading {}", progress.tool)
    } else {
        String::new()
    };
    json_ok(json!({
        "success": true,
        "ytdlp": ytdlp.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "ytDlp": ytdlp.is_some(),
        "ffmpeg": ffmpeg.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "ffmpegOptional": ffmpeg_optional,
        "percent": if progress.status == "downloading" { progress.percent } else { 100 },
        "step": step,
        "error": progress.error,
        "ffmpegError": progress.ffmpeg_error,
        "progress": {
            "status": progress.status,
            "tool": progress.tool,
            "percent": progress.percent,
            "error": progress.error,
        },
    }))
}

/// Kick off the same background `ensureTools` upstream runs at boot. Idempotent:
/// a run already in progress is left alone.
async fn tools_install(State(state): State<Shared>) -> Response {
    crate::tools::ensure_tools(state.clone());
    json_ok(json!({ "success": true, "started": true }))
}

// ------------------------------------------------------------- connectivity

async fn net_check(State(state): State<Shared>) -> Response {
    let started = std::time::Instant::now();
    let client = http_client(&state);
    let ok = client
        .get("https://www.youtube.com")
        .send()
        .await
        .map(|r| r.status().is_success() || r.status().as_u16() < 500)
        .unwrap_or(false);
    let latency = started.elapsed().as_millis();
    json_ok(json!({ "success": true, "online": ok, "latencyMs": latency }))
}

async fn audio_output_info(State(_state): State<Shared>) -> Response {
    json_ok(json!({ "success": true, "name": "Default output", "platform": std::env::consts::OS }))
}

// ------------------------------------------------------------------- proxy

async fn proxy_config(State(state): State<Shared>, Json(body): Json<Value>) -> Response {
    let proxy = body.get("proxyUrl").or_else(|| body.get("proxy")).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if let Ok(mut p) = state.proxy_url.lock() {
        *p = proxy.clone();
    }
    json_ok(json!({ "success": true, "proxyUrl": proxy }))
}

async fn proxy_test(State(_state): State<Shared>, Json(body): Json<Value>) -> Response {
    let proxy = body.get("proxyUrl").or_else(|| body.get("proxy")).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if proxy.is_empty() {
        return json_ok(json!({ "success": false, "error": "no proxy configured" }));
    }
    let started = std::time::Instant::now();
    let result = reqwest::Client::builder()
        .proxy(match reqwest::Proxy::all(&proxy) {
            Ok(p) => p,
            Err(e) => return json_ok(json!({ "success": false, "error": format!("invalid proxy: {e}") })),
        })
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .map(|c| c.get("https://www.youtube.com").send());
    match result {
        Ok(fut) => match fut.await {
            Ok(resp) => json_ok(json!({ "success": resp.status().as_u16() < 500, "latencyMs": started.elapsed().as_millis() })),
            Err(e) => json_ok(json!({ "success": false, "error": e.to_string() })),
        },
        Err(e) => json_ok(json!({ "success": false, "error": e.to_string() })),
    }
}

// ------------------------------------------------------------------ lyrics

async fn save_custom_lyrics(State(_state): State<Shared>, Json(body): Json<Value>) -> Response {
    let title = body.get("title").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let artist = body.get("artist").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let lyrics = body.get("lyrics").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if title.is_empty() || lyrics.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "missing title or lyrics");
    }
    let safe = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect::<String>();
    let dir = util::data_dir().join("custom-lyrics");
    let _ = fs::create_dir_all(&dir);
    let path = dir.join(format!("{} - {}.lrc", safe(&artist).trim(), safe(&title).trim()));
    match fs::write(&path, lyrics) {
        Ok(_) => json_ok(json!({ "success": true, "path": path.to_string_lossy() })),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, &format!("could not save lyrics: {e}")),
    }
}

async fn lyrics(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    json_ok(crate::lyrics::lyrics(&state, &q).await)
}

async fn lyrics_words(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    json_ok(crate::lyrics::lyrics_words(&state, &q).await)
}

// ------------------------------------------------------------- media proxy

async fn image_proxy(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let url = qp(&q, "url");
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return json_err(StatusCode::BAD_REQUEST, "invalid url");
    }
    let referer = qp(&q, "referer");
    let client = http_client(&state);
    let mut req = client.get(&url);
    if !referer.is_empty() {
        req = req.header("Referer", referer.clone());
    }
    match req.send().await {
        Ok(resp) => {
            if let Some(len) = resp.content_length() {
                if len > 20 * 1024 * 1024 {
                    return json_err(StatusCode::PAYLOAD_TOO_LARGE, "image too large");
                }
            }
            let status = resp.status();
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("image/jpeg")
                .to_string();
            match resp.bytes().await {
                Ok(bytes) => Response::builder()
                    .status(if status.is_success() { StatusCode::OK } else { StatusCode::BAD_GATEWAY })
                    .header(axum::http::header::CONTENT_TYPE, content_type)
                    .header(axum::http::header::CACHE_CONTROL, "public, max-age=86400")
                    .body(Body::from(bytes))
                    .unwrap(),
                Err(_) => json_err(StatusCode::BAD_GATEWAY, "failed to read image"),
            }
        }
        Err(_) => json_err(StatusCode::BAD_GATEWAY, "image fetch failed"),
    }
}

async fn audio_proxy(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>, req: Request<Body>) -> Response {
    let url = qp(&q, "url");
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return json_err(StatusCode::BAD_REQUEST, "invalid url");
    }
    let client = http_client(&state);
    let mut out_req = client.get(&url);
    if let Some(range) = req.headers().get(axum::http::header::RANGE) {
        out_req = out_req.header(reqwest::header::RANGE, range.clone());
    }
    match out_req.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::OK);
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("audio/mp4")
                .to_string();
            // Forward length/range metadata â€” WebView2's media pipeline needs
            // them to demux MP4 progressively; without Content-Range the
            // element reports MEDIA_ERR_SRC_NOT_SUPPORTED ("Format error").
            let content_length = resp.content_length();
            let content_range = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok().map(str::to_string));
            let accept_ranges = resp
                .headers()
                .get(reqwest::header::ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok().map(str::to_string));
            let stream = resp.bytes_stream().map(|chunk| {
                chunk.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
            });
            let mut builder = Response::builder()
                .status(status)
                .header(axum::http::header::CONTENT_TYPE, content_type)
                .header(
                    axum::http::header::ACCEPT_RANGES,
                    accept_ranges.unwrap_or_else(|| "bytes".into()),
                );
            if let Some(content_length) = content_length {
                builder = builder.header(axum::http::header::CONTENT_LENGTH, content_length);
            }
            if let Some(content_range) = content_range {
                builder = builder.header(axum::http::header::CONTENT_RANGE, content_range);
            }
            builder.body(Body::from_stream(stream)).unwrap()
        }
        Err(_) => json_err(StatusCode::BAD_GATEWAY, "stream fetch failed"),
    }
}

// --------------------------------------------------------------- defaults

async fn default_settings(State(_state): State<Shared>) -> Response {
    const DEFAULTS: &str = include_str!("../../src/renderer/defaults.json");
    match serde_json::from_str::<Value>(DEFAULTS) {
        Ok(v) => json_ok(json!({ "success": true, "settings": v })),
        Err(_) => json_ok(json!({ "success": true, "settings": {} })),
    }
}

async fn save_default_settings(State(_state): State<Shared>, Json(_body): Json<Value>) -> Response {
    json_ok(json!({ "success": false, "error": "default settings are read-only" }))
}

async fn save_track_source(State(_state): State<Shared>, Json(_body): Json<Value>) -> Response {
    // Source-metadata persistence for downloaded tracks lands with the download cache in a later milestone.
    json_ok(json!({ "success": true }))
}

async fn api_state(State(_state): State<Shared>, Json(_body): Json<Value>) -> Response {
    json_ok(json!({ "success": true }))
}

async fn brand_icon(State(_state): State<Shared>) -> Response {
    json_err(StatusCode::NOT_FOUND, "brand icon is bundled with the app")
}

// --------------------------------------------------------------- playlist import

const SPOTIFY_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const SPOTIFY_PATHFINDER_URL: &str = "https://api-partner.spotify.com/pathfinder/v1/query";
const SPOTIFY_FETCH_PLAYLIST_HASH: &str = "8964e8eafb21aa992a7d951d256d83285c04be2105d209262901de70cb97584a";

fn parse_spotify_playlist_id(input: &str) -> Option<String> {
    let s = input.trim();
    let rest = if let Some(r) = s.strip_prefix("spotify:playlist:") {
        r
    } else {
        let idx = s.find("playlist/")?;
        &s[idx + "playlist/".len()..]
    };
    let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).take(22).collect();
    (id.len() == 22).then_some(id)
}

/// Extract `<script id="__NEXT_DATA__" ...>{json}</script>` from an HTML page.
fn extract_json_script(body: &str, script_id: &str) -> Option<String> {
    let marker = format!("<script id=\"{script_id}\"");
    let start = body.find(&marker)?;
    let after = &body[start..];
    let gt = after.find('>')?;
    let rest = &after[gt + 1..];
    let end = rest.find("</script>")?;
    let slice = rest[..end].trim();
    (!slice.is_empty()).then(|| slice.to_string())
}

fn spotify_track_from_v2(item: &Value) -> Option<Value> {
    let t = item.get("itemV2").and_then(|v| v.get("data")).or_else(|| item.get("item").and_then(|v| v.get("data")))?;
    let title = t.get("name").and_then(|v| v.as_str())?.trim().to_string();
    if title.is_empty() {
        return None;
    }
    let artists = t
        .get("albumOfTrack")
        .and_then(|a| a.get("artists"))
        .and_then(|a| a.get("items"))
        .and_then(|a| a.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|a| a.get("profile").and_then(|p| p.get("name")).and_then(|n| n.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let duration_ms = t
        .get("trackDuration")
        .and_then(|d| d.get("totalMilliseconds"))
        .or_else(|| t.get("duration").and_then(|d| d.get("totalMilliseconds")))
        .or_else(|| t.get("duration_ms"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let cover = t
        .get("albumOfTrack")
        .and_then(|a| a.get("coverArt"))
        .and_then(|c| c.get("sources"))
        .and_then(|s| s.as_array())
        .and_then(|s| s.last())
        .and_then(|s| s.get("url"))
        .and_then(|u| u.as_str())
        .unwrap_or_default()
        .to_string();
    let is_unavailable = t
        .get("playability")
        .and_then(|p| p.get("playable"))
        .and_then(|p| p.as_bool())
        .or_else(|| t.get("isPlayable").and_then(|p| p.as_bool()))
        .map(|playable| !playable)
        .unwrap_or(false);
    Some(json!({
        "title": title,
        "artist": artists,
        "duration": (duration_ms / 1000.0) as u64,
        "thumbnail": cover,
        "cover": cover,
        "isUnavailable": is_unavailable,
    }))
}

fn spotify_track_from_embed(item: &Value) -> Option<Value> {
    let title = item.get("title").or_else(|| item.get("name")).and_then(|v| v.as_str())?.trim().to_string();
    if title.is_empty() {
        return None;
    }
    let artist = item
        .get("subtitle")
        .or_else(|| item.get("artist"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let duration_ms = item
        .get("duration")
        .and_then(|v| v.as_f64())
        .or_else(|| item.get("duration_ms").and_then(|v| v.as_f64()))
        .unwrap_or(0.0);
    let cover = item
        .get("image")
        .or_else(|| item.get("coverArt").and_then(|c| c.get("sources")).and_then(|s| s.as_array()).and_then(|s| s.first()))
        .or_else(|| item.get("coverArt").and_then(|c| c.get("sources")).and_then(|s| s.as_array()).and_then(|s| s.first()).and_then(|s| s.get("url")))
        .and_then(|v| v.as_str().or_else(|| v.get("url").and_then(|u| u.as_str())))
        .unwrap_or_default()
        .to_string();
    let is_unavailable = item
        .get("playability")
        .and_then(|p| p.get("playable"))
        .and_then(|p| p.as_bool())
        .map(|playable| !playable)
        .unwrap_or(false);
    Some(json!({
        "title": title,
        "artist": artist,
        "duration": (duration_ms / 1000.0) as u64,
        "thumbnail": cover,
        "cover": cover,
        "isUnavailable": is_unavailable,
    }))
}

/// Deep-walk the page-data tree for track-like objects (last-resort fallback).
fn collect_tracks_walk(node: &Value, seen: &mut std::collections::HashSet<String>, out: &mut Vec<Value>) {
    match node {
        Value::Array(items) => {
            for item in items {
                collect_tracks_walk(item, seen, out);
            }
        }
        Value::Object(map) => {
            let uri = map.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            let title = map.get("title").or_else(|| map.get("name")).and_then(|v| v.as_str()).unwrap_or("");
            let subtitle = map
                .get("subtitle")
                .or_else(|| map.get("artist"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    map.get("artists").and_then(|a| a.as_array()).map(|items| {
                        items
                            .iter()
                            .filter_map(|a| {
                                a.as_str()
                                    .map(|s| s.to_string())
                                    .or_else(|| a.get("name").or_else(|| a.get("profile").and_then(|p| p.get("name"))).and_then(|v| v.as_str()).map(|s| s.to_string()))
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                });
            let duration = map
                .get("duration")
                .and_then(|d| d.get("totalMilliseconds"))
                .or_else(|| map.get("trackDuration").and_then(|d| d.get("totalMilliseconds")))
                .or_else(|| map.get("duration_ms"))
                .or_else(|| map.get("duration"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if !title.is_empty()
                && (subtitle.is_some() || uri.starts_with("spotify:track:"))
                && (uri.starts_with("spotify:track:") || map.get("entityType").and_then(|v| v.as_str()) == Some("track") || duration > 0.0)
            {
                let key = format!("{}__{}", title, subtitle.clone().unwrap_or_default());
                if seen.insert(key) {
                    let cover = map
                        .get("image")
                        .and_then(|v| v.as_str())
                        .or_else(|| map.get("coverArt").and_then(|c| c.get("sources")).and_then(|s| s.as_array()).and_then(|s| s.first()).and_then(|s| s.get("url")).and_then(|u| u.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let dur_sec = if duration > 1000.0 { (duration / 1000.0) as u64 } else { duration as u64 };
                    out.push(json!({
                        "title": title.trim(),
                        "artist": subtitle.unwrap_or_default().trim(),
                        "duration": dur_sec,
                        "thumbnail": cover,
                        "cover": cover,
                    }));
                }
            }
            for value in map.values() {
                collect_tracks_walk(value, seen, out);
            }
        }
        _ => {}
    }
}

async fn spotify_pathfinder_page(client: &reqwest::Client, token: &str, playlist_id: &str, offset: u64, limit: u64) -> Result<Value, String> {
    let body = json!({
        "variables": { "uri": format!("spotify:playlist:{playlist_id}"), "offset": offset, "limit": limit, "enableWatchFeedEntrypoint": true },
        "operationName": "fetchPlaylist",
        "extensions": { "persistedQuery": { "version": 1, "sha256Hash": SPOTIFY_FETCH_PLAYLIST_HASH } },
    });
    let resp = client
        .post(SPOTIFY_PATHFINDER_URL)
        .header("Authorization", format!("Bearer {token}"))
        .header("Referer", "https://open.spotify.com/")
        .header("User-Agent", SPOTIFY_UA)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("pathfinder request failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("Pathfinder status {}", status.as_u16()));
    }
    resp.json::<Value>().await.map_err(|e| format!("pathfinder parse error: {e}"))
}

async fn spotify_import(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let raw = qp(&q, "url");
    let playlist_id = match parse_spotify_playlist_id(&raw) {
        Some(id) => id,
        None => return json_ok(json!({ "success": false, "code": "spotify_error", "error": "Invalid Spotify playlist URL" })),
    };

    let client = http_client(&state);
    let embed_url = format!("https://open.spotify.com/embed/playlist/{playlist_id}");
    let resp = match client.get(&embed_url).header("User-Agent", SPOTIFY_UA).header("Referer", "https://open.spotify.com/").header("Accept-Language", "en-US,en;q=0.9").send().await {
        Ok(r) => r,
        Err(_) => return json_ok(json!({ "success": false, "code": "spotify_error", "error": "Network error connecting to Spotify" })),
    };
    let status = resp.status().as_u16();
    match status {
        200 => {}
        404 => return json_ok(json!({ "success": false, "code": "spotify_not_found", "error": "Playlist not found" })),
        403 => return json_ok(json!({ "success": false, "code": "spotify_forbidden", "error": "Spotify refused request (403)" })),
        429 => return json_ok(json!({ "success": false, "code": "spotify_rate_limited", "error": "Spotify rate limited (429)" })),
        other => return json_ok(json!({ "success": false, "code": "spotify_error", "error": format!("Spotify returned status {other}") })),
    }
    let body = resp.text().await.unwrap_or_default();

    let json_str = match extract_json_script(&body, "__NEXT_DATA__") {
        Some(s) => s,
        None => {
            let is_private = body.to_lowercase().contains("private");
            return json_ok(json!({
                "success": false,
                "code": if is_private { "spotify_forbidden" } else { "spotify_error" },
                "error": if is_private { "Private playlist" } else { "Could not parse playlist" },
            }));
        }
    };
    let data: Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(_) => return json_ok(json!({ "success": false, "code": "spotify_error", "error": "Could not parse playlist" })),
    };
    let page_props = data.pointer("/props/pageProps").cloned().unwrap_or(json!({}));
    if page_props.get("status").and_then(|v| v.as_u64()) == Some(404) {
        return json_ok(json!({ "success": false, "code": "spotify_not_found", "error": "Playlist not found" }));
    }
    let entity = page_props.pointer("/state/data/entity").cloned().unwrap_or(json!({}));
    let name = entity
        .get("name")
        .or_else(|| page_props.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or("Spotify Playlist")
        .to_string();
    let cover = entity
        .pointer("/images/0/url")
        .or_else(|| entity.pointer("/coverArt/sources/0/url"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            body.split("og:image").nth(1).and_then(|part| part.split("content=\"").nth(1)).and_then(|part| part.split('"').next()).map(|s| s.to_string())
        })
        .unwrap_or_default();

    // Preferred path: anonymous access token from the embed + pathfinder pagination
    let access_token = page_props.pointer("/state/settings/session/accessToken").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut tracks: Vec<Value> = Vec::new();
    let mut total = 0u64;
    let mut limited = false;
    if !access_token.is_empty() {
        match spotify_pathfinder_page(&client, &access_token, &playlist_id, 0, 100).await {
            Ok(first) => {
                let content = first.pointer("/data/playlistV2/content").cloned().unwrap_or(json!({}));
                let total_count = content.get("totalCount").and_then(|v| v.as_u64()).unwrap_or(0);
                if let Some(items) = content.get("items").and_then(|v| v.as_array()) {
                    let mut all = items.clone();
                    let mut offset = items.len() as u64;
                    let mut guard = 0;
                    while offset < total_count && guard < 100 {
                        guard += 1;
                        match spotify_pathfinder_page(&client, &access_token, &playlist_id, offset, 100).await {
                            Ok(page) => match page.pointer("/data/playlistV2/content/items").and_then(|v| v.as_array()).cloned() {
                                Some(page_items) => {
                                    if page_items.is_empty() {
                                        break;
                                    }
                                    offset += page_items.len() as u64;
                                    all.extend(page_items);
                                }
                                None => break,
                            },
                            Err(_) => break,
                        }
                    }
                    tracks = all.iter().filter_map(spotify_track_from_v2).collect();
                    total = total_count.max(tracks.len() as u64);
                } else {
                    limited = true;
                }
            }
            Err(_) => {
                limited = true;
            }
        }
    } else {
        limited = true;
    }

    // Fallback: embed trackList (max 100), then generic walk
    if tracks.is_empty() {
        if let Some(items) = entity.get("trackList").and_then(|v| v.as_array()) {
            tracks = items.iter().take(100).filter_map(spotify_track_from_embed).collect();
        }
    }
    if tracks.is_empty() {
        let mut seen = std::collections::HashSet::new();
        collect_tracks_walk(&page_props, &mut seen, &mut tracks);
    }

    if tracks.is_empty() {
        return json_ok(json!({ "success": false, "code": "spotify_error", "error": "No tracks found in this playlist" }));
    }
    let count = tracks.len() as u64;
    json_ok(json!({
        "success": true,
        "name": name,
        "cover": cover,
        "tracks": tracks,
        "total": if total > 0 { total } else { count },
        "limited": limited,
    }))
}

async fn playlist_import(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let url = qp(&q, "url");
    if parse_spotify_playlist_id(&url).is_some() {
        return spotify_import(State(state), Query(q)).await;
    }
    // YouTube / SoundCloud playlists via yt-dlp flat extraction
    let args = vec![
        "-J".to_string(),
        "--flat-playlist".to_string(),
        "--no-warnings".to_string(),
        url.clone(),
    ];
    let state_for_task = state.clone();
    let out = match tokio::task::spawn_blocking(move || run_yt_dlp(&state_for_task, &args, 90)).await
    {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return json_ok(json!({ "success": false, "error": e })),
        Err(e) => return json_ok(json!({ "success": false, "error": format!("playlist task failed: {e}") })),
    };
    let info: Value = match serde_json::from_str(&out) {
        Ok(v) => v,
        Err(e) => return json_ok(json!({ "success": false, "error": format!("yt-dlp output parse error: {e}") })),
    };
    let entries = info.get("entries").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let tracks: Vec<Value> = entries
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if id.is_empty() {
                return None;
            }
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
            let artist = item
                .get("uploader")
                .or_else(|| item.get("channel"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown artist")
                .to_string();
            let duration = item.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0) as u64;
            let thumb = item
                .get("thumbnails")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.last().cloned())
                .and_then(|t| t.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
                .or_else(|| item.get("thumbnail").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .unwrap_or_default();
            let url = item
                .get("webpage_url")
                .or_else(|| item.get("url"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("https://www.youtube.com/watch?v={id}"));
            Some(json!({
                "id": id,
                "title": title,
                "artist": artist,
                "duration": duration,
                "thumbnail": thumb,
                "cover": thumb,
                "url": url,
                "platform": "ytmusic",
            }))
        })
        .collect();
    if tracks.is_empty() {
        return json_ok(json!({ "success": false, "error": "No tracks found in this playlist" }));
    }
    let name = info
        .get("title")
        .or_else(|| info.get("playlist_title"))
        .and_then(|v| v.as_str())
        .unwrap_or("Playlist")
        .to_string();
    let cover = entries
        .first()
        .and_then(|item| item.get("thumbnails").and_then(|v| v.as_array()).and_then(|arr| arr.last().cloned()))
        .and_then(|t| t.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    json_ok(json!({ "success": true, "name": name, "cover": cover, "tracks": tracks, "total": tracks.len() }))
}

// ------------------------------------------------------------------ stubs

async fn stub_unported(Query(q): Query<HashMap<String, String>>) -> Response {
    unported(&q)
}

async fn stub_empty_list() -> Response {
    unported_list()
}

async fn fallback(req: Request<Body>) -> Response {
    let path = req.uri().path().to_string();
    if path.starts_with("/api/") {
        util::log_line("STUB", &format!("unported route hit: {path}"));
        json_ok(json!({ "success": false, "error": format!("{NOT_PORTED}: {path}") }))
    } else {
        json_err(StatusCode::NOT_FOUND, "not found")
    }
}

// ------------------------------------------------------------------ router

pub fn build_router(state: Shared) -> Router {
    Router::new()
        .route("/api/search", get(search))
        .route("/api/stream-info", get(stream_info))
        .route("/api/local-track", get(local_track))
        .route("/api/library-file", get(library_file))
        .route("/api/local-thumbnail", get(local_thumbnail))
        .route("/api/library", get(library_list))
        .route("/api/library-add", post(library_add))
        .route("/api/library-update", post(library_update))
        .route("/api/library-remove", post(library_remove))
        .route("/api/offline-library", get(offline_library))
        .route("/api/offline-delete", get(offline_delete).post(offline_delete))
        .route("/api/clear-offline-library", post(clear_offline_library))
        .route("/api/check-cache", get(check_cache))
        .route("/api/download", get(download).post(download))
        .route("/api/tools/status", get(tools_status))
        .route("/api/tools-status", get(tools_status))
        .route("/api/tools/install", post(tools_install))
        .route("/api/tools-install", post(tools_install))
        .route("/api/net-check", get(net_check))
        .route("/api/audio-output-info", get(audio_output_info))
        .route("/api/image-proxy", get(image_proxy))
        .route("/api/audio-proxy", get(audio_proxy))
        .route("/api/lyrics", get(lyrics))
        .route("/api/save-custom-lyrics", post(save_custom_lyrics))
        .route("/api/lyrics-stream", get(stub_unported))
        .route("/api/proxy-config", post(proxy_config))
        .route("/api/proxy-test", post(proxy_test))
        .route("/api/default-settings", get(default_settings))
        .route("/api/save-default-settings", post(save_default_settings))
        .route("/api/save-track-source", post(save_track_source))
        .route("/api/state", post(api_state))
        .route("/api/brand-icon", get(brand_icon))
        .route("/api/trending", get(trending))
        .route("/api/hot", get(hot))
        .route("/api/top-chart", get(top_chart))
        .route("/api/vibe", post(vibe))
        .route("/api/track-analysis", get(stub_unported))
        .route("/api/artist", get(artist))
        .route("/api/prefetch", get(stub_unported))
        .route("/api/albums/search", get(stub_empty_list))
        .route("/api/playlists/search", get(stub_empty_list))
        .route("/api/albums/tracks", get(stub_empty_list))
        .route("/api/album", get(stub_empty_list))
        .route("/api/album-tracks", get(stub_empty_list))
        .route("/api/track-cover", get(stub_unported))
        .route("/api/track-credits", get(stub_unported))
        .route("/api/check-playlist-unavailable", post(stub_empty_list))
        .route("/api/playlist-import", get(playlist_import).post(playlist_import))
        .route("/api/spotify-import", get(spotify_import).post(spotify_import))
        .route("/api/apple-import", get(stub_empty_list).post(stub_empty_list))
        .route("/api/yandex-import", get(stub_empty_list).post(stub_empty_list))
        .route("/api/tiktok-import", get(stub_empty_list).post(stub_empty_list))
        .route("/api/vk-import", get(stub_empty_list).post(stub_empty_list))
        .route("/api/spotify-auth-start", get(stub_unported))
        .route("/api/spotify-callback", get(stub_unported))
        .route("/api/spotify-status", get(stub_unported))
        .route("/api/spotify-logout", post(stub_unported))
        .route("/api/spotify-library", get(stub_empty_list))
        .route("/api/lastfm/token", get(stub_unported))
        .route("/api/lastfm/session", get(stub_unported))
        .route("/api/lastfm/mobile-session", get(stub_unported))
        .route("/api/lastfm/now-playing", get(stub_unported))
        .route("/api/lastfm/scrobble", post(stub_unported))
        .route("/api/lastfm/{*rest}", get(stub_unported).post(stub_unported))
        .route("/api/lyrics-words", get(lyrics_words))
        .route("/api/translate", post(stub_unported))
        .route("/api/i18n-batch", post(stub_unported))
        .route("/api/devsize-memes", get(stub_unported))
        .route("/api/encode", post(stub_unported))
        .route("/api/tmp-file", get(stub_unported))
        .fallback(fallback)
        .with_state(state)
}

/// Serve the embedded API on an already-bound std TcpListener (called from a dedicated thread).
pub fn router_with_cors(state: Shared) -> Router {
    use tower_http::cors::{Any, CorsLayer};
    build_router(state).layer(
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
            .expose_headers(Any),
    )
}

pub fn serve(listener: std::net::TcpListener, state: Shared) {
    std::thread::spawn(move || {
        util::log_line("INFO", "serve thread started");
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                util::log_line("ERROR", &format!("failed to create tokio runtime: {e}"));
                return;
            }
        };
        runtime.block_on(async move {
            util::log_line("INFO", "serve runtime up");
            let listener = match TcpListener::from_std(listener) {
                Ok(l) => {
                    util::log_line("INFO", "listener converted");
                    l
                }
                Err(e) => {
                    util::log_line("ERROR", &format!("failed to convert listener: {e}"));
                    return;
                }
            };
            let app = router_with_cors(state);
            util::log_line("INFO", "axum serve starting");
            if let Err(e) = axum::serve(listener, app).await {
                util::log_line("ERROR", &format!("embedded server crashed: {e}"));
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::parse_range_header;

    #[test]
    fn parses_open_ended_range() {
        assert_eq!(parse_range_header(Some("bytes=0-"), 1000), Ok(Some((0, 999))));
        assert_eq!(
            parse_range_header(Some("bytes=500-"), 1000),
            Ok(Some((500, 999)))
        );
    }

    #[test]
    fn parses_bounded_and_suffix_ranges() {
        assert_eq!(
            parse_range_header(Some("bytes=100-199"), 1000),
            Ok(Some((100, 199)))
        );
        assert_eq!(
            parse_range_header(Some("bytes=-100"), 1000),
            Ok(Some((900, 999)))
        );
        // End beyond EOF is clamped, as the HTTP spec requires.
        assert_eq!(
            parse_range_header(Some("bytes=900-5000"), 1000),
            Ok(Some((900, 999)))
        );
    }

    #[test]
    fn rejects_unsatisfiable_ranges() {
        assert_eq!(parse_range_header(Some("bytes=1000-"), 1000), Err(()));
        assert_eq!(parse_range_header(Some("bytes=500-100"), 1000), Err(()));
        assert_eq!(parse_range_header(Some("bytes=-0"), 1000), Err(()));
        assert_eq!(parse_range_header(Some("items=0-10"), 1000), Err(()));
        assert_eq!(parse_range_header(Some("bytes=0-"), 0), Err(()));
        assert_eq!(parse_range_header(None, 0), Ok(None));
    }
}
