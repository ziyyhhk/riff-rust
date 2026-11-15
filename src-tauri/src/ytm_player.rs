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
    let client = state.http();
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

/// Returns itag-compatible url for the requested video (or None if every client fails).
pub async fn direct_audio_url(video_id: &str, quality: &str, state: &Arc<AppState>) -> Option<String> {
    if video_id.chars().count() != 11
        || !video_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let visitor = visitor_data(state).await.unwrap_or_default();
    let client = state.http();

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
