//! SoundCloud direct API access (search + progressive stream resolution).
//!
//! Upstream spawns yt-dlp for both search and stream resolution (~10-25 s per track).
//! SoundCloud exposes the same data over plain JSON once you hold the public
//! `client_id` that their web player fetches from its JS bundles. This module
//! performs that lookup once, caches it, and then resolves tracks in a few
//! hundred milliseconds.
//!
//! Every failure here falls back to the yt-dlp path in `server.rs`, so a change on
//! SoundCloud's side breaks nothing; it just behaves like before.

use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::state::AppState;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const HOME_PAGE: &str = "https://soundcloud.com/";
const API_BASE: &str = "https://api-v2.soundcloud.com";
const CLIENT_CACHE_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// Return the cached client_id when warm, else scrape a fresh one from the JS
/// bundles SoundCloud serves on its front page.
pub async fn client_id(state: &Arc<AppState>) -> Option<String> {
    if let Ok(slot) = state.soundcloud_client_id.lock() {
        if let Some((id, at)) = slot.as_ref() {
            if at.elapsed() < CLIENT_CACHE_TTL {
                return Some(id.clone());
            }
        }
    }
    let id = scrape_client_id(state).await?;
    if let Ok(mut slot) = state.soundcloud_client_id.lock() {
        *slot = Some((id.clone(), Instant::now()));
    }
    Some(id)
}

async fn scrape_client_id(state: &Arc<AppState>) -> Option<String> {
    let client = state.http();
    let started = Instant::now();
    let html = client
        .get(HOME_PAGE)
        .header("User-Agent", UA)
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let mut assets: Vec<String> = Vec::new();
    let mut rest = html.as_str();
    while let Some(idx) = rest.find("https://a-v2.sndcdn.com/assets/") {
        let tail = &rest[idx..];
        let end = tail
            .find(|c: char| c == '"' || c == '\'' || c == ')' || c.is_whitespace())
            .unwrap_or(tail.len());
        let url = &tail[..end];
        if url.ends_with(".js") && !assets.iter().any(|a| a == url) {
            assets.push(url.to_string());
        }
        rest = &tail[end..];
    }
    if assets.is_empty() {
        util_log("WARN", "soundcloud: no JS assets on homepage");
        return None;
    }
    // Bundles ship newest-last; try them in reverse order.
    for asset in assets.iter().rev().take(12) {
        let Ok(resp) = client
            .get(asset)
            .header("User-Agent", UA)
            .send()
            .await
        else {
            continue;
        };
        let Ok(js) = resp.text().await else {
            continue;
        };
        if let Some(id) = extract_client_id(&js) {
            util_log(
                "INFO",
                &format!("soundcloud: client_id acquired in {}ms", started.elapsed().as_millis()),
            );
            return Some(id);
        }
    }
    util_log("WARN", "soundcloud: client_id not found in JS assets");
    None
}

/// Find `client_id:<32 alnum>` in a JS bundle without pulling in a regex crate.
fn extract_client_id(js: &str) -> Option<String> {
    for needle in ["client_id:\"", "client_id=\"", "client_id: \"", "client_id:'"] {
        let Some(start) = js.find(needle) else {
            continue;
        };
        let mut candidate = String::with_capacity(32);
        for ch in js[start + needle.len()..].chars() {
            if ch.is_ascii_alphanumeric() && candidate.len() < 32 {
                candidate.push(ch);
            } else {
                break;
            }
        }
        if candidate.len() == 32 {
            return Some(candidate);
        }
    }
    None
}

fn util_log(level: &str, msg: &str) {
    crate::util::log_line(level, msg);
}

fn urlencode(value: &str) -> String {
    crate::util::percent_encode(value)
}

/// Search SoundCloud tracks. Returns the same shape `ytdlp_search` produces.
pub async fn search_tracks(
    state: &Arc<AppState>,
    query: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let started = Instant::now();
    let cid = client_id(state).await.ok_or_else(|| "soundcloud unavailable".to_string())?;
    let limit = limit.clamp(1, 24);
    let url = format!(
        "{API_BASE}/search/tracks?q={}&limit={}&offset=0&client_id={}",
        urlencode(query),
        limit,
        cid
    );
    let response = state
        .http()
        .get(&url)
        .header("User-Agent", UA)
        .header("Origin", "https://soundcloud.com")
        .header("Referer", "https://soundcloud.com/")
        .send()
        .await
        .map_err(|e| format!("soundcloud search request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("soundcloud search status {}", response.status().as_u16()));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("soundcloud search parse error: {e}"))?;
    let collection = payload
        .get("collection")
        .and_then(Value::as_array)
        .ok_or_else(|| "soundcloud search missing collection".to_string())?;
    let mut tracks = Vec::with_capacity(collection.len());
    for item in collection {
        if item.get("kind").and_then(Value::as_str) != Some("track") {
            continue;
        }
        if item.get("streamable").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let Some(id) = item.get("id").and_then(|v| v.as_u64()) else {
            continue;
        };
        let id = id.to_string();
        let title = item.get("title").and_then(Value::as_str).unwrap_or("Unknown").trim().to_string();
        if title.is_empty() {
            continue;
        }
        let artist = item
            .get("user")
            .and_then(|u| u.get("username"))
            .and_then(Value::as_str)
            .unwrap_or("Unknown Artist")
            .trim()
            .to_string();
        let duration = item
            .get("duration")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            / 1000;
        let thumbnail = item
            .get("artwork_url")
            .and_then(Value::as_str)
            .map(|u| u.replacen("-large", "-t500x500", 1))
            .unwrap_or_default();
        let Some(url) = item.get("permalink_url").and_then(Value::as_str) else {
            continue;
        };
        tracks.push(json!({
            "id": id,
            "title": title,
            "artist": artist,
            "duration": duration,
            "thumbnail": thumbnail,
            "url": url,
            "platform": "soundcloud",
            "source": "soundcloud",
        }));
    }
    util_log(
        "INFO",
        &format!(
            "soundcloud: search {:?} -> {} tracks in {}ms",
            query,
            tracks.len(),
            started.elapsed().as_millis()
        ),
    );
    Ok(tracks)
}

#[derive(Debug, Clone)]
pub struct ResolvedStream {
    pub url: String,
    pub is_hls: bool,
}

/// Exchange a transcoding descriptor for the actual CDN URL. SoundCloud returns
/// a JSON envelope (`{"url": "..."}`) for both progressive and HLS presets.
async fn fetch_transcoding_url(
    state: &Arc<AppState>,
    transcoding: &Value,
    cid: &str,
) -> Option<String> {
    let tc_url = transcoding.get("url").and_then(Value::as_str)?;
    let sep = if tc_url.contains('?') { '&' } else { '?' };
    let stream_resp = state
        .http()
        .get(format!("{tc_url}{sep}client_id={cid}"))
        .header("User-Agent", UA)
        .header("Origin", "https://soundcloud.com")
        .header("Referer", "https://soundcloud.com/")
        .send()
        .await
        .ok()?;
    if !stream_resp.status().is_success() {
        util_log(
            "WARN",
            &format!(
                "soundcloud: transcoding fetch status {}",
                stream_resp.status().as_u16()
            ),
        );
        return None;
    }
    let stream_json: Value = stream_resp.json().await.ok()?;
    stream_json
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Resolve a SoundCloud permalink to something the renderer can play.
///
/// Most tracks expose a progressive MP3. Some (e.g. newer uploads) only have
/// HLS renditions; those used to return `None`, which forced the slow yt-dlp
/// fallback and then timed out. The HLS playlist is CORS-enabled and the
/// renderer already has hls.js, so hand its `playlist.m3u8` back directly.
pub async fn resolve_stream(
    state: &Arc<AppState>,
    permalink: &str,
) -> Option<ResolvedStream> {
    let started = Instant::now();
    let cid = client_id(state).await?;
    let resolve_url = format!(
        "{API_BASE}/resolve?url={}&client_id={}",
        urlencode(permalink),
        cid
    );
    let response = state
        .http()
        .get(&resolve_url)
        .header("User-Agent", UA)
        .header("Origin", "https://soundcloud.com")
        .header("Referer", "https://soundcloud.com/")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        util_log(
            "WARN",
            &format!(
                "soundcloud: resolve status {} for {permalink}",
                response.status().as_u16()
            ),
        );
        return None;
    }
    let payload: Value = response.json().await.ok()?;
    let transcodings = payload
        .pointer("/media/transcodings")
        .and_then(Value::as_array)?;

    let protocol = |tc: &Value| {
        tc.pointer("/format/protocol")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };

    let mut progressive = transcodings
        .iter()
        .filter(|tc| {
            protocol(tc) == "progressive"
                && tc
                    .pointer("/format/mime_type")
                    .and_then(Value::as_str)
                    .map(|m| m.contains("mpeg"))
                    .unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();
    // Prefer the 128 kbps MP3 when several progressive variants exist.
    progressive.sort_by_key(|tc| {
        tc.get("preset")
            .and_then(Value::as_str)
            .map(|p| if p.contains("mp3_1_0") { 0u8 } else { 1u8 })
            .unwrap_or(1u8)
    });
    for chosen in &progressive {
        if let Some(url) = fetch_transcoding_url(state, chosen, &cid).await {
            util_log(
                "INFO",
                &format!(
                    "soundcloud: progressive resolve in {}ms",
                    started.elapsed().as_millis()
                ),
            );
            return Some(ResolvedStream {
                url,
                is_hls: false,
            });
        }
    }

    let mut hls = transcodings
        .iter()
        .filter(|tc| protocol(tc) == "hls")
        .cloned()
        .collect::<Vec<_>>();
    // AAC 160k is the best quality/CPU tradeoff; fall back to AAC 96k, then any
    // HLS rendition (the last one is sometimes an Opus/abr_sq stream).
    hls.sort_by_key(|tc| {
        let preset = tc.get("preset").and_then(Value::as_str).unwrap_or("");
        if preset.contains("aac_160k") {
            0u8
        } else if preset.contains("aac_96k") {
            1u8
        } else if preset.contains("mp3") {
            2u8
        } else {
            3u8
        }
    });
    for chosen in &hls {
        if let Some(url) = fetch_transcoding_url(state, chosen, &cid).await {
            util_log(
                "INFO",
                &format!(
                    "soundcloud: hls resolve ({}) in {}ms",
                    chosen
                        .get("preset")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown"),
                    started.elapsed().as_millis()
                ),
            );
            return Some(ResolvedStream { url, is_hls: true });
        }
    }

    util_log(
        "WARN",
        &format!(
            "soundcloud: no playable transcoding for {permalink} ({} presets)",
            transcodings.len()
        ),
    );
    None
}
