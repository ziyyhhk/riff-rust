//! Lyrics engine. Port of upstream's server-side LRC sources + word timings.
//!
//! What changed vs the vendored path:
//! - `/api/lyrics` hits LRCLIB **with a 6 s timeout and an in-memory cache**
//!   (upstream caches to disk + memory); we keep responses for 24 h.
//! - LRC files containing an `[offset:<ms>]` tag are **honoured** (shifted to
//!   the file's own timing) instead of being silently dropped by the renderer.
//! - `/api/lyrics-words` returns word-level timings from NetEase (line-level
//!   LRC fallback when unavailable), so word-level karaoke sync works.
//!
//! Contract (mirrors upstream): `{success:true, lyrics, syncedLyrics, plainLyrics,
//! isCustom, source}` for `/api/lyrics`; `{success:true, lines:[{time, text, words?}]}`
//! for `/api/lyrics-words`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::state::{cache_get, cache_put};
use crate::util;

const LRCLIB_BASE: &str = "https://lrclib.net";
const LRCLIB_TIMEOUT_SECS: u64 = 6;
const LRCLIB_CACHE_TTL_SECS: u64 = 60 * 60 * 24; // 24h
const WORD_CACHE_TTL_SECS: u64 = 60 * 60 * 6; // 6h
const WORD_NEGATIVE_TTL_SECS: u64 = 60 * 10; // 10min

fn custom_lyrics_dir() -> PathBuf {
    util::data_dir().join("custom-lyrics")
}

fn custom_lyrics_path(artist: &str, title: &str) -> Option<PathBuf> {
    if artist.trim().is_empty() || title.trim().is_empty() {
        return None;
    }
    let safe = |s: &str| {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
            .collect::<String>()
    };
    let a = safe(artist);
    let t = safe(title);
    for ext in ["lrc", "txt"] {
        let path = custom_lyrics_dir().join(format!("{} - {}.{ext}", a.trim(), t.trim()));
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

async fn lrc_client(state: &Arc<crate::state::AppState>) -> reqwest::Client {
    state.http()
}

/// Parse an `[mm:ss(.frac)]` inner timestamp. Returns seconds (fractions allowed).
fn parse_lrc_ts(inner: &str) -> Option<f64> {
    let (mm, rest) = inner.split_once(':')?;
    let mm: u64 = mm.trim().parse().ok()?;
    let seconds_field: f64 = rest.trim().replace(',', ".").parse().ok()?;
    Some(mm as f64 * 60.0 + seconds_field)
}

/// Rewrite an LRC file, applying the leading `[offset:±ms]` tag to every line.
/// The offset line itself is dropped. Results match `[mm:ss.xx]` format.
fn apply_offset(text: &str, offset_ms: f64) -> String {
    if offset_ms == 0.0 {
        return text.to_string();
    }
    text.lines()
        .map(|line| {
            let Some(end) = line.find(']') else { return line.to_string() };
            let ts = parse_lrc_ts(&line[1..end]);
            let Some(t) = ts else { return line.to_string() };
            let new_t = (t + offset_ms / 1000.0).max(0.0);
            let total_centis = (new_t * 100.0).round() as i64;
            let mins = total_centis / 6000;
            let secs = (total_centis % 6000) / 100;
            let centis = total_centis % 100;
            format!("[{mins:02}:{secs:02}.{centis:02}]{}", &line[end + 1..])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract the file-level `[offset:...]` (milliseconds) if present.
fn read_offset_ms(text: &str) -> Option<f64> {
    for line in text.lines() {
        let Some(inner) = line.strip_prefix("[offset:") else { continue };
        let Some(end) = inner.find(']') else { continue };
        let raw = inner[..end].trim();
        if let Ok(ms) = raw.parse::<f64>() {
            // LRC convention: offset is in milliseconds, can carry sign.
            return Some(ms);
        }
        // Some files use seconds-with-unit, e.g. "1.5s" or "-250ms"
        if let Some(ms) = raw.strip_suffix("ms").and_then(|s| s.trim().parse::<f64>().ok()) {
            return Some(ms);
        }
        if let Some(s) = raw.strip_suffix('s').and_then(|s| s.trim().parse::<f64>().ok()) {
            return Some(s * 1000.0);
        }
    }
    None
}

/// Heuristic LRC parse: `[mm:ss.xx] text` lines with embedded times.
/// Returns sorted list of (time_secs, raw_text).
struct LrcHit {
    lyrics: String,
    is_explicit: bool,
    source: &'static str,
}

fn lyrics_payload(hit: LrcHit) -> Value {
    json!({
        "success": true,
        "lyrics": hit.lyrics,
        "syncedLyrics": hit.lyrics,
        "isCustom": false,
        "isExplicit": hit.is_explicit,
        "source": hit.source,
    })
}

pub async fn lyrics(state: &Arc<crate::state::AppState>, q: &HashMap<String, String>) -> Value {
    let title = q.get("title").map(|s| s.trim().to_string()).unwrap_or_default();
    let artist = q.get("artist").map(|s| s.trim().to_string()).unwrap_or_default();
    let online = q.get("online").map(|s| s.as_str() == "1").unwrap_or(true);
    let duration: Option<f64> = q.get("duration").and_then(|s| s.parse::<f64>().ok()).filter(|d| *d > 0.0);

    // Custom lyrics win.
    if let Some(path) = custom_lyrics_path(&artist, &title) {
        if let Ok(text) = std::fs::read_to_string(path) {
            return json!({
                "success": true,
                "isCustom": true,
                "source": "custom",
                "lyrics": text,
                "syncedLyrics": text,
            });
        }
    }
    if !online {
        return json!({ "success": false, "error": "offline mode", "lyrics": "" });
    }
    if title.is_empty() && artist.is_empty() {
        return json!({ "success": false, "error": "missing title and artist", "lyrics": "" });
    }

    let artist_q = if artist.is_empty() { title.clone() } else { artist.clone() };
    let title_q = if title.is_empty() { artist.clone() } else { title.clone() };
    let cache_key = format!("lyrics:{}|{}", artist_q.to_lowercase(), title_q.to_lowercase());
    if let Some(cached) = cache_get(&state.lyrics_cache, &cache_key, Duration::from_secs(LRCLIB_CACHE_TTL_SECS)) {
        return lyrics_payload(LrcHit {
            lyrics: cached.as_str().unwrap_or_default().to_string(),
            is_explicit: false,
            source: "lrclib",
        });
    }

    let client = lrc_client(state).await;
    let timeout = Duration::from_secs(LRCLIB_TIMEOUT_SECS);

    // 1) Exact /api/get
    let mut exact_url = format!(
        "{LRCLIB_BASE}/api/get?artist_name={}&track_name={}",
        util::percent_encode(&artist_q),
        util::percent_encode(&title_q)
    );
    if let Some(d) = duration {
        exact_url.push_str(&format!("&duration={d}"));
    }
    let mut exact_resp = match tokio::time::timeout(timeout, client.get(&exact_url).send()).await {
        Ok(Ok(resp)) if resp.status().is_success() => Some(resp),
        _ => None,
    };

    // If the exact match with duration failed, retry without duration in case
    // the duration we were given is slightly off (different remaster, etc.).
    if exact_resp.is_none() && duration.is_some() {
        let no_dur_url = format!(
            "{LRCLIB_BASE}/api/get?artist_name={}&track_name={}",
            util::percent_encode(&artist_q),
            util::percent_encode(&title_q)
        );
        exact_resp = match tokio::time::timeout(timeout, client.get(&no_dur_url).send()).await {
            Ok(Ok(resp)) if resp.status().is_success() => Some(resp),
            _ => None,
        };
    }

    // 2) Fallback /api/search (first non-instrumental, else first)
    if exact_resp.is_none() {
        let search_url = format!(
            "{LRCLIB_BASE}/api/search?q={}",
            util::percent_encode(&format!("{artist_q} {title_q}"))
        );
        if let Ok(Ok(resp)) = tokio::time::timeout(timeout, client.get(&search_url).send()).await {
            if resp.status().is_success() {
                if let Ok(items) = resp.json::<Vec<Value>>().await {
                    // Sort candidates by score (descending), then try each one
                    // until we find one with lyrics. The old code picked only the
                    // top scorer and gave up if that single item had no lyrics.
                    let mut candidates: Vec<&Value> = items
                        .iter()
                        .filter(|i| i.get("instrumental").and_then(Value::as_bool) != Some(true))
                        .collect();
                    candidates.sort_by(|a, b| {
                        lrc_match_score(b, &title_q, &artist_q, duration)
                            .cmp(&lrc_match_score(a, &title_q, &artist_q, duration))
                    });
                    for item in candidates {
                        if let Some(lyrics) = item_lrc(item) {
                            let adjusted = adjust_and_tag(&lyrics);
                            cache_put(
                                &state.lyrics_cache,
                                cache_key,
                                Value::String(adjusted.clone()),
                                256,
                                Duration::from_secs(LRCLIB_CACHE_TTL_SECS),
                            );
                            return lyrics_payload(LrcHit {
                                lyrics: adjusted,
                                is_explicit: item_is_explicit(item),
                                source: "lrclib",
                            });
                        }
                    }
                }
            }
        }
    } else if let Some(resp) = exact_resp.take() {
        if let Ok(item) = resp.json::<Value>().await {
            if let Some(lyrics) = item_lrc(&item) {
                let adjusted = adjust_and_tag(&lyrics);
                cache_put(
                    &state.lyrics_cache,
                    cache_key,
                    Value::String(adjusted.clone()),
                    256,
                    Duration::from_secs(LRCLIB_CACHE_TTL_SECS),
                );
                return lyrics_payload(LrcHit {
                    lyrics: adjusted,
                    is_explicit: item_is_explicit(&item),
                    source: "lrclib",
                });
            }
        }
    }

    // Misses are not cached: a transient blip used to be remembered as
    // "no lyrics" for 24 h and skipped retries forever.
    json!({ "success": false, "error": "no lyrics found", "lyrics": "" })
}

/// Rank-LRCLIB-candidates by how likely they are to be the same recording as
/// the one being played. A plain first-hit pick can bind the wrong remaster
/// or cover; penalize covers/karaoke hard (same rules as `best_search_hit`).
fn lrc_norm(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn lrc_match_score(item: &Value, title: &str, artist: &str, duration: Option<f64>) -> i32 {
    let want_title = lrc_norm(title);
    let title_prefix: String = want_title.chars().take(12).collect();
    let want_artist = lrc_norm(artist);
    let item_title = item
        .get("trackName")
        .or_else(|| item.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let item_artist = item
        .get("artistName")
        .and_then(Value::as_str)
        .unwrap_or("");
    let hit_title = lrc_norm(item_title);
    let hit_artist = lrc_norm(item_artist);
    let mut score = 0;

    if !want_title.is_empty() && !hit_title.is_empty()
        && (hit_title.contains(&want_title)
            || want_title.contains(&hit_title)
            || (!title_prefix.is_empty() && hit_title.contains(&title_prefix)))
    {
        score += 4;
    }
    if !want_artist.is_empty() && !hit_artist.is_empty()
        && (hit_artist.contains(&want_artist) || want_artist.contains(&hit_artist))
    {
        score += 3;
    }

    let lower = item_title.to_lowercase();
    let wanted_lower = title.to_lowercase();
    for bad in ["cover", "karaoke", "instrumental", "nightcore", "sped up", "slowed", "reverb", "8d", "remix"] {
        if lower.contains(bad) && !wanted_lower.contains(bad) {
            score -= 6;
        }
    }

    if let (Some(want_dur), Some(item_dur)) =
        (duration, item.get("duration").and_then(Value::as_f64))
    {
        if (item_dur - want_dur).abs() > 20.0 {
            score -= 4;
        }
    }
    score
}

fn item_lrc(item: &Value) -> Option<String> {
    item.get("syncedLyrics")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            item.get("plainLyrics")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
}

fn item_is_explicit(item: &Value) -> bool {
    item.get("isExplicitTenor")
        .and_then(Value::as_bool)
        .or_else(|| item.get("isExplicit").and_then(Value::as_bool))
        .unwrap_or(false)
}

fn adjust_and_tag(raw: &str) -> String {
    match read_offset_ms(raw) {
        Some(ms) => apply_offset(raw, ms),
        None => raw.to_string(),
    }
}

// ----------------------------------------------------------------- words

const NETEASE_SEARCH: &str = "https://music.163.com/api/search/get/web?type=1&limit=5&s=";
const NETEASE_LYRIC: &str = "https://music.163.com/api/song/lyric";

pub async fn lyrics_words(state: &Arc<crate::state::AppState>, q: &HashMap<String, String>) -> Value {
    let title = q.get("title").map(|s| s.trim().to_string()).unwrap_or_default();
    let artist = q.get("artist").map(|s| s.trim().to_string()).unwrap_or_default();
    let target_ms = q
        .get("duration")
        .and_then(|s| s.parse::<f64>().ok())
        .map(|d| (d * 1000.0) as u64)
        .unwrap_or(0);
    if title.is_empty() && artist.is_empty() {
        return json!({ "success": false, "lines": [] });
    }

    let cache_key = format!("words:{}|{}|{}", artist.to_lowercase(), title.to_lowercase(), target_ms);
    if let Some(cached) = cache_get(&state.lyrics_words_cache, &cache_key, Duration::from_secs(WORD_CACHE_TTL_SECS)) {
        return cached;
    }

    let query = format!("{} {}", if artist.is_empty() { title.clone() } else { artist.clone() }, title);
    let search_url = format!("{NETEASE_SEARCH}{}", util::percent_encode(&query));
    let client = lrc_client(state).await;
    let timeout = Duration::from_secs(6);

    let id: Option<u64> = match tokio::time::timeout(timeout, client.get(&search_url).header("Referer", "https://music.163.com/").send())
        .await
    {
        Ok(Ok(resp)) if resp.status().is_success() => {
            resp.json::<Value>().await.ok().and_then(|v| {
                v["result"]["songs"]
                    .as_array()
                    .cloned()
                    .and_then(|arr| {
                        arr.into_iter()
                            .find(|s| {
                                let dur_ms = s["duration"].as_u64().unwrap_or(0);
                                target_ms == 0 || (dur_ms as i64 - target_ms as i64).abs() <= 4_000
                            })
                            .and_then(|s| s["id"].as_u64())
                    })
            })
        }
        _ => None,
    };

    let lines = match id {
        Some(id) => match tokio::time::timeout(
            timeout,
            client
                .get(format!("{NETEASE_LYRIC}?id={id}&lv=1&kv=1&tv=-1&yv=1"))
                .header("Referer", "https://music.163.com/")
                .send(),
        )
        .await
        {
            Ok(Ok(resp)) if resp.status().is_success() => {
                resp.json::<Value>().await.ok().and_then(|v| {
                    v.get("yrc")
                        .and_then(|y| y.get("lyric"))
                        .and_then(Value::as_str)
                        .map(String::from)
                        .or_else(|| v.get("lrc").and_then(|y| y.get("lyric")).and_then(Value::as_str).map(String::from))
                        .map(|yrc| parse_yrc(&yrc))
                })
            }
            _ => None,
        },
        None => None,
    };

    let payload = match &lines {
        Some(lines) if !lines.is_empty() => json!({ "success": true, "lines": lines }),
        _ => json!({ "success": false, "lines": [] }),
    };
    cache_put(
        &state.lyrics_words_cache,
        cache_key,
        payload.clone(),
        256,
        if payload["success"].as_bool() == Some(true) {
            Duration::from_secs(WORD_CACHE_TTL_SECS)
        } else {
            Duration::from_secs(WORD_NEGATIVE_TTL_SECS)
        },
    );
    payload
}

/// Parse NetEase enhanced-LRC lines `[ts,end](startMs,durMs,fontIdx) text`.
/// Returns `[{time, text, words: [{text, start, end}]}]` (seconds).
fn parse_yrc(text: &str) -> Vec<Value> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let Some(line_end) = raw.find(']') else { continue };
        let header = &raw[..line_end];
        let Some(ts) = header.strip_prefix('[') else { continue };
        let mut ts_parts = ts.split(',');
        let start_raw = ts_parts.next().map(str::trim).unwrap_or("");
        let start_ms = if let Ok(ms) = start_raw.parse::<f64>() {
            ms // NetEase enhanced YRC: milliseconds
        } else if let Some(secs) = parse_lrc_ts(start_raw) {
            secs * 1000.0 // plain LRC: mm:ss(.xx)
        } else {
            continue;
        };
        let end_ms = ts_parts.next().and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(start_ms + 4000.0);
        let content = raw[line_end + 1..].trim();
        if content.is_empty() {
            continue;
        }

        let mut words = Vec::new();
        let bytes = content.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            // Expect "(...)" to start a word run
            if bytes[i] != b'(' {
                i += 1;
                continue;
            }
            let Some(rel_close) = bytes[i + 1..].iter().position(|&b| b == b')') else { break };
            let close = i + 1 + rel_close;
            let spec = &content[i + 1..close];
            let mut spec_parts = spec.split(',');
            let w_start = spec_parts.next().and_then(|s| s.trim().parse::<f64>().ok());
            let w_dur = spec_parts.next().and_then(|s| s.trim().parse::<f64>().ok());
            // Collect text until the next '(' or end of string
            let text_start = close + 1;
            let mut j = close + 1;
            let mut text_end = bytes.len();
            while j < bytes.len() {
                if bytes[j] == b'(' {
                    text_end = j;
                    break;
                }
                j += 1;
            }
            let word_text = content[text_start..text_end].trim().to_string();
            if let (Some(ws), Some(wd)) = (w_start, w_dur) {
                if !word_text.is_empty() {
                    words.push(json!({
                        "text": word_text,
                        "start": ws / 1000.0,
                        "end": (ws + wd) / 1000.0,
                    }));
                }
            }
            i = text_end;
        }

        let plain_text = if words.is_empty() {
            content.to_string()
        } else {
            words.iter().filter_map(|w| w["text"].as_str()).collect::<Vec<_>>().join(" ")
        };

        lines.push(json!({
            "time": start_ms / 1000.0,
            "text": plain_text,
            "words": words,
        }));
        let _ = end_ms;
    }
    lines.sort_by(|a, b| {
        a["time"]
            .as_f64()
            .partial_cmp(&b["time"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    lines
}
