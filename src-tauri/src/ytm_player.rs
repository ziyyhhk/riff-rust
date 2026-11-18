//! Direct YouTube Music stream resolution — inner `/v1/player` endpoint.
//!
//! Upstream uses this before falling back to yt-dlp. It resolves to an
//! unciphered `https://rr*.googlevideo.com/videoplayback?...` URL in ~200-500 ms,
//! no signature-cipher evaluation, no subprocess at all.

use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use serde_json::{Value, json};

use crate::state::AppState;
use std::sync::Arc;

const VISITOR_TTL_SECS: u64 = 60 * 30;
const VISITOR_FETCH_TIMEOUT_SECS: u64 = 5;
const PLAYER_TIMEOUT_SECS: u64 = 6;
const GOOGLEBOT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

struct ClientSpec {
    name: &'static str,
    version: &'static str,
    user_agent: &'static str,
    os_name: Option<&'static str>,
    os_version: Option<&'static str>,
    device_make: Option<&'static str>,
    device_model: Option<&'static str>,
    android_sdk_version: Option<u32>,
    host: &'static str,
    use_visitor_header: bool,
}

const CLIENTS: &[ClientSpec] = &[
    ClientSpec {
        name: "IOS",
        version: "20.10.4",
        user_agent: "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)",
        os_name: Some("iOS"),
        os_version: Some("18.3.2.22D60"),
        device_make: Some("Apple"),
        device_model: Some("iPhone16,2"),
        android_sdk_version: None,
        host: "www.youtube.com",
        use_visitor_header: true,
    },
    ClientSpec {
        name: "ANDROID_MUSIC",
        version: "7.27.52",
        user_agent: "com.google.android.apps.youtube.music/7.27.52 (Linux; U; Android 14; en_US; Quest 3) gzip",
        os_name: None,
        os_version: None,
        device_make: None,
        device_model: None,
        android_sdk_version: Some(34),
        host: "music.youtube.com",
        use_visitor_header: false,
    },
];

static VISITOR_DATA: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// Parse the visitor data token out of `sw.js_data` (regex-free, stdlib-only).
fn extract_visitor(txt: &str) -> Option<String> {
    let mut idx = 0;
    while idx < txt.len() {
        let tail = &txt[idx..];
        let rel = tail.find(['"', '\''])?;
        let start = idx + rel + 1;
        let after = &txt[start..];
        if !after.starts_with('C') && !after.starts_with('g') && !after.starts_with('t') {
            idx = start;
            continue;
        }
        let end = after.find(['"', '\''])?;
        let slice = &after[..end];
        let ok = slice.len() >= 10
            && slice.len() <= 64
            && slice
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '%');
        if ok {
            return Some(slice.to_string());
        }
        idx = start;
    }
    None
}

pub async fn visitor_data(state: &Arc<AppState>) -> Option<String> {
    {
        let guard = VISITOR_DATA.lock().ok()?;
        if let Some((v, ts)) = guard.as_ref() {
            if ts.elapsed() < Duration::from_secs(VISITOR_TTL_SECS) {
                return Some(v.clone());
            }
        }
    }
    let client = state.streaming_http();
    let resp = client
        .get("https://www.youtube.com/sw.js_data")
        .header("User-Agent", GOOGLEBOT_UA)
        .timeout(Duration::from_secs(VISITOR_FETCH_TIMEOUT_SECS))
        .send()
        .await
        .ok()?;
    let txt = resp.text().await.ok()?;
    let matched = extract_visitor(&txt)?;
    if let Ok(mut guard) = VISITOR_DATA.lock() {
        *guard = Some((matched.clone(), Instant::now()));
    }
    Some(matched)
}

fn pick_audio_url(data: &Value, quality: &str) -> Option<String> {
    let mut all: Vec<Value> = Vec::new();
    if let Some(list) = data.get("streamingData").and_then(|v| v.get("adaptiveFormats")).and_then(Value::as_array) {
        all.extend(list.iter().cloned());
    }
    if let Some(list) = data.get("streamingData").and_then(|v| v.get("formats")).and_then(Value::as_array) {
        all.extend(list.iter().cloned());
    }
    let mut audio = all
        .into_iter()
        .filter_map(|f| {
            let mime = f.get("mimeType").and_then(|v| v.as_str())?;
            if !mime.starts_with("audio/") {
                return None;
            }
            let url = f.get("url").and_then(|v| v.as_str())?;
            if url.is_empty() {
                return None;
            }
            if f.get("signatureCipher").is_some() || f.get("cipher").is_some() {
                return None;
            }
            let bitrate = f.get("bitrate").and_then(Value::as_u64).unwrap_or(0);
            Some((mime.to_string(), bitrate, url.to_string()))
        })
        .collect::<Vec<_>>();

    if audio.is_empty() {
        return None;
    }
    if quality == "low" || quality == "worst" {
        audio.sort_by_key(|(_, bitrate, _)| *bitrate);
    } else if quality == "high" || quality == "max" {
        audio.sort_by(|a, b| b.1.cmp(&a.1));
    } else {
        // prefer m4a (mp4) by bitrate; fall back to webm/opus; else best bitrate
        let m4a: Vec<_> = audio.iter().filter(|(m, _, _)| m.contains("mp4")).cloned().collect();
        let webm: Vec<_> = audio.iter().filter(|(m, _, _)| m.contains("webm")).cloned().collect();
        audio = if !m4a.is_empty() { m4a } else if !webm.is_empty() { webm } else { audio };
        audio.sort_by(|a, b| b.1.cmp(&a.1));
    }
    audio.into_iter().next().map(|(_, _, u)| u)
}

// ------------------------------------------------------------ visionos fast path
//
// The iOS client's signed URL only serves its first ~256 KB and 403s on every
// later byte, which is why every uncached track needed a 10-16 s yt-dlp
// fallback. The VISIONOS inner client (the one yt-dlp tries first, JS-less)
// returns an unrestricted URL, but only when the request carries the current
// player JS's `signatureTimestamp`. That value changes whenever YouTube ships
// a new player, so it is scraped from watch page -> base.js and cached 24 h.
const VISIONOS_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const PLAYER_CTX_TTL_SECS: u64 = 60 * 60 * 24;
const PAGE_TIMEOUT_SECS: u64 = 30;

static VISIONOS_CONTEXT: Mutex<Option<(u32, String, Instant)>> = Mutex::new(None);

fn extract_between<'a>(hay: &'a str, start: &str, end: char) -> Option<&'a str> {
    let idx = hay.find(start)?;
    let tail = &hay[idx + start.len()..];
    let end_idx = tail.find(end)?;
    Some(&tail[..end_idx])
}

/// GET a URL and return at most `max_bytes` of the body. Uses a Range request;
/// if the origin ignores it and streams the full body, the connection is hung
/// up right after `max_bytes` are collected — the watch page embeds everything
/// we need in its first ~62 KB, and the player JS carries `signatureTimestamp`
/// at ~65 KB, so the whole 1.3 MB/3 MB bodies never have to cross the wire.
async fn fetch_prefix(
    client: &reqwest::Client,
    url: &str,
    user_agent: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let resp = client
        .get(url)
        .header("User-Agent", user_agent)
        .header("Range", format!("bytes=0-{}", max_bytes.saturating_sub(1)))
        .timeout(timeout)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() && resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return None;
    }
    let mut buf: Vec<u8> = Vec::with_capacity(max_bytes);
    let mut stream = resp.bytes_stream();
    while (buf.len() as u64) < max_bytes as u64 {
        match futures_util::StreamExt::next(&mut stream).await {
            Some(Ok(chunk)) => {
                let take = (max_bytes - buf.len()).min(chunk.len());
                buf.extend_from_slice(&chunk[..take]);
            }
            Some(Err(_)) | None => break,
        }
    }
    if buf.is_empty() {
        return None;
    }
    Some(buf)
}

/// www.youtube.com/watch…embeds the player file as "/s/player/<key>/<variant>/base.js".
fn extract_player_url(page: &str) -> Option<String> {
    let idx = page.find("/s/player/")?;
    let marker = "base.js";
    let end = page[idx..].find(marker)? + idx + marker.len();
    Some(format!("https://www.youtube.com{}", &page[idx..end]))
}

fn extract_signature_timestamp(player_js: &str) -> Option<u32> {
    let idx = player_js.find("signatureTimestamp")?;
    let digits: String = player_js[idx..]
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<u32>().ok()
}

fn extract_visitor_data(page: &str) -> Option<String> {
    extract_between(page, "\"visitorData\":\"", '"').map(str::to_string)
}

/// Full-text fallback for files whose markers were not found in the prefix cap.
async fn fetch_text(client: &reqwest::Client, url: &str) -> Option<String> {
    let resp = client
        .get(url)
        .header("User-Agent", VISIONOS_UA)
        .timeout(Duration::from_secs(PAGE_TIMEOUT_SECS))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

const PREFIX_PAGE_BYTES: usize = 256 * 1024;
const PREFIX_PLAYER_BYTES: usize = 768 * 1024;

/// (signatureTimestamp, visitorData) from the watch page + player JS prefix, cached for a day.
async fn visionos_context(video_id: &str, client: &reqwest::Client) -> Option<(u32, Option<String>)> {
    {
        let guard = VISIONOS_CONTEXT.lock().ok()?;
        if let Some((sts, visitor, cached_at)) = guard.as_ref() {
            if cached_at.elapsed() < Duration::from_secs(PLAYER_CTX_TTL_SECS) {
                return Some((*sts, Some(visitor.clone())));
            }
        }
    }
    let page_url = format!("https://www.youtube.com/watch?v={video_id}&bpctr=9999999999&has_verified=1");
    let page_bytes = fetch_prefix(client, &page_url, VISIONOS_UA, PREFIX_PAGE_BYTES, Duration::from_secs(PAGE_TIMEOUT_SECS)).await?;
    let page = String::from_utf8_lossy(&page_bytes).to_string();
    let page = if page.find("/s/player/").is_some() && page.find("\"visitorData\":\"").is_some() {
        page
    } else {
        // Prefix did not carry the markers (page layout changed) — fetch the whole thing.
        fetch_text(client, &page_url).await?
    };
    let player_url = extract_player_url(&page)?;
    let js_bytes = fetch_prefix(client, &player_url, VISIONOS_UA, PREFIX_PLAYER_BYTES, Duration::from_secs(PAGE_TIMEOUT_SECS)).await?;
    let js = String::from_utf8_lossy(&js_bytes).to_string();
    let sts = match extract_signature_timestamp(&js) {
        Some(sts) => sts,
        None => {
            // sts not in the prefix — pull the full file before giving up.
            let full = fetch_text(client, &player_url).await?;
            extract_signature_timestamp(&full)?
        }
    };
    let visitor = extract_visitor_data(&page).unwrap_or_default();
    if let Ok(mut guard) = VISIONOS_CONTEXT.lock() {
        *guard = Some((sts, visitor.clone(), Instant::now()));
    }
    if visitor.is_empty() {
        return Some((sts, None));
    }
    Some((sts, Some(visitor)))
}

/// Roughly 0.3 s when the player-context cache is warm (it refills once a day),
/// about 2-3 s when cold. The returned URL works with unrestricted byte ranges.
pub async fn visionos_audio_url(
    video_id: &str,
    quality: &str,
    state: &Arc<AppState>,
) -> Option<String> {
    if video_id.chars().count() != 11
        || !video_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let client = state.streaming_http();
    let (sts, visitor) = visionos_context(video_id, &client).await?;
    let mut client_ctx = json!({
        "clientName": "VISIONOS",
        "clientVersion": "1.02",
        "hl": "en",
        "gl": "US",
        "deviceMake": "Apple",
        "deviceModel": "RealityDevice17,1",
        "osName": "visionOS",
        "osVersion": "26.5.23O471",
        "userAgent": VISIONOS_UA,
        "timeZone": "UTC",
        "utcOffsetMinutes": 0,
    });
    if let Some(v) = &visitor {
        client_ctx["visitorData"] = json!(v);
    }
    let body = json!({
        "videoId": video_id,
        "context": { "client": client_ctx },
        "playbackContext": {
            "contentPlaybackContext": {
                "html5Preference": "HTML5_PREF_WANTS",
                "signatureTimestamp": sts,
            }
        },
        "contentCheckOk": true,
        "racyCheckOk": true,
    });
    let mut req = client
        .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.youtube.com")
        .header("User-Agent", VISIONOS_UA)
        .header("X-YouTube-Client-Name", "101")
        .header("X-YouTube-Client-Version", "1.02");
    if let Some(v) = &visitor {
        req = req.header("X-Goog-Visitor-Id", v.as_str());
    }
    let Some(resp) = req
        .timeout(Duration::from_secs(PLAYER_TIMEOUT_SECS))
        .json(&body)
        .send()
        .await
        .ok()
    else {
        return None;
    };
    if !resp.status().is_success() {
        return None;
    }
    let data: Value = resp.json::<Value>().await.ok()?;
    if data.get("playabilityStatus").and_then(|v| v.get("status")).and_then(Value::as_str) != Some("OK") {
        return None;
    }
    pick_audio_url(&data, quality)
}

// --------------------------------------------------------------------------------

/// Returns itag-compatible url for the requested video (or None if every client fails).
pub async fn direct_audio_url(video_id: &str, quality: &str, state: &Arc<AppState>) -> Option<String> {
    if video_id.chars().count() != 11
        || !video_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let visitor = visitor_data(state).await.unwrap_or_default();
    let client = state.streaming_http();

    for client_spec in CLIENTS {
        let mut ctx_client = json!({
            "clientName": client_spec.name,
            "clientVersion": client_spec.version,
            "hl": "en",
            "gl": "US",
        });
        if let Some(os) = client_spec.os_name { ctx_client["osName"] = json!(os); }
        if let Some(osv) = client_spec.os_version { ctx_client["osVersion"] = json!(osv); }
        if let Some(mk) = client_spec.device_make { ctx_client["deviceMake"] = json!(mk); }
        if let Some(mdl) = client_spec.device_model { ctx_client["deviceModel"] = json!(mdl); }
        if let Some(sdk) = client_spec.android_sdk_version { ctx_client["androidSdkVersion"] = json!(sdk); }
        if !visitor.is_empty() { ctx_client["visitorData"] = json!(visitor); }

        let body = json!({
            "videoId": video_id,
            "context": { "client": ctx_client },
            "contentCheckOk": true,
            "racyCheckOk": true,
        });

        let req = client
            .post(format!("https://{}/youtubei/v1/player?prettyPrint=false", client_spec.host))
            .header("Content-Type", "application/json")
            .header("User-Agent", client_spec.user_agent)
            .header("X-Goog-Api-Format-Version", "2");
        let req = if client_spec.use_visitor_header && !visitor.is_empty() {
            req.header("X-Goog-Visitor-Id", visitor.as_str())
        } else {
            req
        };
        let Some(resp) = req
            .timeout(Duration::from_secs(PLAYER_TIMEOUT_SECS))
            .json(&body)
            .send()
            .await
            .ok()
        else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(data) = resp.json::<Value>().await else {
            continue;
        };
        if data.get("playabilityStatus").and_then(|v| v.get("status")).and_then(Value::as_str) != Some("OK") {
            continue;
        }
        if let Some(url) = pick_audio_url(&data, quality) {
            return Some(url);
        }
    }
    None
}
