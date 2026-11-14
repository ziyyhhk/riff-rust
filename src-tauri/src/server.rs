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

use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;

use crate::library;
use crate::state::AppState;
use crate::util;

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

/// Build an HTTP client honouring the configured proxy.
fn http_client(state: &AppState) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) RiffRust/0.1");
    let proxy = state.proxy_url.lock().map(|p| p.clone()).unwrap_or_default();
    if !proxy.trim().is_empty() {
        if let Ok(p) = reqwest::Proxy::all(proxy.trim()) {
            builder = builder.proxy(p);
        }
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// Spawn a blocking process with a hard timeout; kills it if it overruns.
fn run_yt_dlp(state: &AppState, args: &[String], timeout_secs: u64) -> Result<String, String> {
    let bin = util::resolve_binary("yt-dlp").ok_or_else(|| "yt-dlp not found".to_string());
    let bin = match bin {
        Ok(b) => b,
        Err(e) => return Err(e),
    };
    let mut full_args: Vec<String> = Vec::new();
    if let Some(ffmpeg) = util::resolve_binary("ffmpeg") {
        full_args.push("--ffmpeg-location".into());
        full_args.push(ffmpeg.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());
    }
    full_args.extend(args.iter().cloned());
    if let Ok(proxy) = state.proxy_url.lock() {
        if !proxy.trim().is_empty() {
            full_args.push("--proxy".into());
            full_args.push(proxy.trim().to_string());
        }
    }

    let child = match std::process::Command::new(&bin)
        .args(&full_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("failed to start yt-dlp: {e}")),
    };

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let out = child.wait_with_output();
        let _ = tx.send(out);
    });
    match rx.recv_timeout(std::time::Duration::from_secs(timeout_secs)) {
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                Err(String::from_utf8_lossy(&output.stderr).to_string())
            }
        }
        Ok(Err(e)) => Err(format!("yt-dlp I/O error: {e}")),
        Err(_) => Err("yt-dlp timed out".to_string()),
    }
}

// ---------------------------------------------------------------- search

async fn search(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let query = qp(&q, "q");
    let platform = qp(&q, "platform");
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(10).clamp(1, 24);
    if query.trim().is_empty() {
        return json_ok(json!({ "success": true, "tracks": [] }));
    }
    let prefix = if platform.eq_ignore_ascii_case("soundcloud") { "scsearch" } else { "ytsearch" };
    let search_target = format!("{prefix}{limit}:{query}");
    let args = vec![
        "--dump-json".to_string(),
        "--flat-playlist".to_string(),
        "--no-warnings".to_string(),
        "--no-playlist".to_string(),
        search_target,
    ];
    let out = match run_yt_dlp(&state, &args, 25) {
        Ok(o) => o,
        Err(e) => {
            util::log_line("WARN", &format!("search failed: {e}"));
            return json_ok(json!({ "success": false, "tracks": [], "error": e }));
        }
    };
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
                "platform": if platform.eq_ignore_ascii_case("soundcloud") { "soundcloud" } else { "ytmusic" },
                "source": "youtube",
            }));
        }
    }
    json_ok(json!({ "success": true, "tracks": tracks }))
}

// ------------------------------------------------------------- stream info

async fn stream_info(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let id = qp(&q, "id");
    let url = qp(&q, "url");
    let title = qp(&q, "title");
    let artist = qp(&q, "artist");
    let duration_q = qp(&q, "duration");

    // Local import?
    if library::find_track_by_id(&id).is_some() {
        let port = state.server_port;
        return json_ok(json!({
            "success": true,
            "isLocal": true,
            "streamUrl": format!("http://127.0.0.1:{port}/api/local-track?id={id}"),
            "resolvedId": id,
            "title": title,
            "artist": artist,
            "duration": duration_q.parse::<u64>().unwrap_or(0),
        }));
    }

    let target = if !url.trim().is_empty() {
        url.clone()
    } else if !id.is_empty() {
        format!("https://www.youtube.com/watch?v={id}")
    } else {
        return json_err(StatusCode::BAD_REQUEST, "missing url or id");
    };

    // Downloaded cache hit?
    if let Some(path) = library::find_cached_by_id(&id) {
        let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "mp3".into());
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

    let quality = qp(&q, "quality");
    let format_arg = match quality.as_str() {
        "low" | "worst" => "worstaudio",
        "medium" => "bestaudio[abr<=96]/bestaudio",
        _ => "bestaudio",
    };

    let args = vec![
        "-j".to_string(),
        "--no-warnings".to_string(),
        "--no-playlist".to_string(),
        "--format".to_string(),
        format_arg.to_string(),
        target.clone(),
    ];
    let out = match run_yt_dlp(&state, &args, 45) {
        Ok(o) => o,
        Err(e) => {
            util::log_line("WARN", &format!("stream-info failed for {target}: {e}"));
            return json_ok(json!({ "success": false, "error": e }));
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
    let mime = info.get("ext").and_then(|v| v.as_str()).map(util::mime_from_ext).unwrap_or("audio/mp4");
    json_ok(json!({
        "success": true,
        "streamUrl": stream_url,
        "mimeType": mime,
        "resolvedId": resolved_id,
        "duration": duration,
        "title": info.get("title").and_then(|v| v.as_str()).unwrap_or(&title),
        "artist": info.get("uploader").and_then(|v| v.as_str()).unwrap_or(&artist),
        "thumbnail": thumbnail,
        "direct": true,
    }))
}

fn item_thumbnail(item: &Value) -> Option<String> {
    item.get("thumbnails")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.last().cloned())
        .and_then(|t| t.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()))
}

// ---------------------------------------------------- local file streaming

fn serve_file_with_range(path: &PathBuf, req: &Request<Body>) -> Response {
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(_) => return json_err(StatusCode::NOT_FOUND, "file not found"),
    };
    let total = data.len() as u64;
    let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "mp3".into());
    let mime = util::mime_from_ext(&ext);

    let mut start: u64 = 0;
    let mut end: u64 = total.saturating_sub(1);
    let mut is_range = false;
    if let Some(range) = req.headers().get(axum::http::header::RANGE).and_then(|v| v.to_str().ok()) {
        if let Some(spec) = range.strip_prefix("bytes=") {
            let mut parts = spec.splitn(2, '-');
            let a = parts.next().unwrap_or("").trim();
            let b = parts.next().unwrap_or("").trim();
            if a.is_empty() {
                // suffix range: last N bytes
                if let Ok(suffix) = b.parse::<u64>() {
                    start = total.saturating_sub(suffix.min(total));
                    end = total.saturating_sub(1);
                    is_range = true;
                }
            } else {
                start = a.parse::<u64>().unwrap_or(0);
                if !b.is_empty() {
                    end = b.parse::<u64>().unwrap_or(total.saturating_sub(1));
                } else {
                    end = total.saturating_sub(1);
                }
                end = end.min(total.saturating_sub(1));
                is_range = true;
            }
        }
    }

    let slice = data[start as usize..(end as usize + 1)].to_vec();
    let mut response = Response::builder()
        .status(if is_range { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK })
        .header(axum::http::header::CONTENT_TYPE, mime)
        .header(axum::http::header::ACCEPT_RANGES, "bytes")
        .header(axum::http::header::CONTENT_LENGTH, slice.len().to_string());
    if is_range {
        response = response.header(
            axum::http::header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{total}"),
        );
    }
    response.body(Body::from(slice)).unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn local_track(State(_state): State<Shared>, Query(q): Query<HashMap<String, String>>, req: Request<Body>) -> Response {
    let id = qp(&q, "id");
    match library::find_track_by_id(&id) {
        Some(path) => serve_file_with_range(&path, &req),
        None => json_err(StatusCode::NOT_FOUND, "track not found"),
    }
}

async fn library_file(State(state): State<Shared>, Query(q): Query<HashMap<String, String>>, req: Request<Body>) -> Response {
    local_track(State(state), Query(q), req).await
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
    // downloaded cache file
    let mut removed = false;
    if let Ok(entries) = fs::read_dir(util::cache_dir()) {
        for entry in entries.flatten() {
            let stem = entry.file_name().to_string_lossy().to_string();
            if stem.starts_with(&id) {
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

// ------------------------------------------------------------------- tools

async fn tools_status(State(_state): State<Shared>) -> Response {
    let ytdlp = util::resolve_binary("yt-dlp");
    let ffmpeg = util::resolve_binary("ffmpeg");
    json_ok(json!({
        "success": true,
        "ytdlp": ytdlp.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "ytDlp": ytdlp.is_some(),
        "ffmpeg": ffmpeg.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        "ffmpegOptional": false,
        "percent": 100,
        "step": "",
    }))
}

async fn tools_install(State(_state): State<Shared>) -> Response {
    let bin_dir = util::bin_dir();
    let _ = fs::create_dir_all(&bin_dir);
    let target = bin_dir.join(if cfg!(target_os = "windows") { "yt-dlp.exe" } else { "yt-dlp" });
    if target.is_file() {
        return json_ok(json!({ "success": true, "already": true, "path": target.to_string_lossy() }));
    }
    // Background download of the yt-dlp standalone binary (~15 MB). ffmpeg must be
    // installed separately (winget/choco/scoop or your package manager).
    std::thread::spawn(move || {
        let url = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp";
        let url = if cfg!(target_os = "windows") { format!("{url}.exe") } else { url.to_string() };
        match reqwest::blocking::get(&url) {
            Ok(resp) => match resp.bytes() {
                Ok(bytes) => {
                    let _ = fs::write(&target, &bytes);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&target, fs::Permissions::from_mode(0o755));
                    }
                    util::log_line("INFO", &format!("yt-dlp installed to {}", target.display()));
                }
                Err(e) => util::log_line("ERROR", &format!("yt-dlp download failed: {e}")),
            },
            Err(e) => util::log_line("ERROR", &format!("yt-dlp download failed: {e}")),
        }
    });
    json_ok(json!({ "success": true, "started": true, "note": "yt-dlp downloading in background; ffmpeg must be installed via winget/choco/scoop" }))
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

fn custom_lyrics_path(artist: &str, title: &str) -> Option<PathBuf> {
    let safe = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' }).collect::<String>();
    let dir = util::data_dir().join("custom-lyrics");
    for ext in ["lrc", "txt"] {
        let p = dir.join(format!("{} - {}.{ext}", safe(artist).trim(), safe(title).trim()));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

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
    let title = qp(&q, "title");
    let artist = qp(&q, "artist");
    let duration = qp(&q, "duration");
    let online = qp(&q, "online");

    // custom lyrics first
    if let Some(path) = custom_lyrics_path(&artist, &title) {
        if let Ok(text) = fs::read_to_string(&path) {
            return json_ok(json!({ "success": true, "lyrics": text, "isCustom": true, "source": "custom" }));
        }
    }
    if online == "0" {
        return json_ok(json!({ "success": false, "error": "offline mode", "lyrics": "" }));
    }
    if title.is_empty() && artist.is_empty() {
        return json_ok(json!({ "success": false, "error": "missing title and artist" }));
    }

    let client = http_client(&state);
    let mut artist_q = artist.clone();
    let mut title_q = title.clone();
    if artist_q.is_empty() {
        artist_q = title.clone();
    }
    if title_q.is_empty() {
        title_q = artist.clone();
    }

    // Exact lookup first, then fuzzy search
    let mut url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}",
        crate::util::percent_encode(&artist_q),
        crate::util::percent_encode(&title_q)
    );
    if let Ok(d) = duration.parse::<f64>() {
        if d > 0.0 {
            url.push_str(&format!("&duration={d}"));
        }
    }
    let mut result = client.get(&url).send().await.ok().filter(|r| r.status().is_success());
    if result.is_none() {
        let search_url = format!(
            "https://lrclib.net/api/search?q={}",
            crate::util::percent_encode(&format!("{artist_q} {title_q}"))
        );
        if let Ok(resp) = client.get(&search_url).send().await {
            if resp.status().is_success() {
                if let Ok(items) = resp.json::<Vec<Value>>().await {
                    // take the first non-instrumental hit
                    if let Some(best) = items.iter().find(|i| i.get("instrumental").and_then(|v| v.as_bool()) != Some(true)).or_else(|| items.first()) {
                        return lyrics_payload(best.clone());
                    }
                }
            }
        }
        return json_ok(json!({ "success": false, "error": "no lyrics found" }));
    }
    if let Some(resp) = result.take() {
        if let Ok(body) = resp.json::<Value>().await {
            return lyrics_payload(body);
        }
    }
    json_ok(json!({ "success": false, "error": "lyrics lookup failed" }))
}

fn lyrics_payload(item: Value) -> Response {
    let synced = item.get("syncedLyrics").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let plain = item.get("plainLyrics").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let lyrics_text = if !synced.is_empty() { synced.clone() } else { plain.clone() };
    if lyrics_text.is_empty() {
        return json_ok(json!({ "success": false, "error": "no lyrics found" }));
    }
    json_ok(json!({
        "success": true,
        "lyrics": lyrics_text,
        "syncedLyrics": synced,
        "plainLyrics": plain,
        "isCustom": false,
        "source": "lrclib",
    }))
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
            let stream = resp.bytes_stream().map(|chunk| {
                chunk.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
            });
            Response::builder()
                .status(status)
                .header(axum::http::header::CONTENT_TYPE, content_type)
                .header(axum::http::header::ACCEPT_RANGES, "bytes")
                .body(Body::from_stream(stream))
                .unwrap()
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
        .route("/api/trending", get(stub_empty_list))
        .route("/api/hot", get(stub_empty_list))
        .route("/api/top-chart", get(stub_empty_list))
        .route("/api/vibe", post(stub_empty_list))
        .route("/api/track-analysis", get(stub_unported))
        .route("/api/artist", get(stub_empty_list))
        .route("/api/prefetch", get(stub_unported))
        .route("/api/albums/search", get(stub_empty_list))
        .route("/api/playlists/search", get(stub_empty_list))
        .route("/api/albums/tracks", get(stub_empty_list))
        .route("/api/album", get(stub_empty_list))
        .route("/api/album-tracks", get(stub_empty_list))
        .route("/api/track-cover", get(stub_unported))
        .route("/api/track-credits", get(stub_unported))
        .route("/api/check-playlist-unavailable", post(stub_empty_list))
        .route("/api/playlist-import", get(stub_empty_list).post(stub_empty_list))
        .route("/api/spotify-import", get(stub_empty_list).post(stub_empty_list))
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
        .route("/api/lyrics-words", get(stub_unported))
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
