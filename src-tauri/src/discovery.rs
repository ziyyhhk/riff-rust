//! Discovery / recommendation backend, ported from upstream Riff's
//! `server.js`: `/api/vibe`, `/api/trending`, `/api/hot`, `/api/top-chart`,
//! `/api/artist`.
//!
//! Data sources (same as upstream, anonymous): YouTube Music's inner API
//! (`next` for RDAMVM mixes, `browse` for FEmusic_charts, `search`), Deezer's
//! public chart/search/artist endpoints, iTunes RSS charts, and Last.fm's
//! public API. No credentials are required.
//!
//! Behavioural contract kept from upstream:
//! - All five endpoints return HTTP 200 with `{success:false, tracks:[]}` (or
//!   `{success:false}` for artist) on failure instead of a 5xx.
//! - Caches: vibe 20 min, chart/country/trending 30 min, hot 1 h.
//! - Synthetic `listeners` are deterministic (hash-based), not real counts.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::{json, Value};

use crate::server::Shared;
use crate::state::{cache_get, cache_put};
use crate::{taste, util, ytm};

const YTM_API_BASE: &str = "https://music.youtube.com/youtubei/v1";
const LASTFM_KEY: &str = "4b4f73bda181868353f9b438604adf52";

fn ytm_body(client: &str, extra: Value) -> Value {
    let mut map = match extra {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    map.insert(
        "context".to_string(),
        json!({
            "client": {
                "clientName": client,
                "clientVersion": "1.20250101.01.00",
                "hl": "en",
                "gl": "US"
            }
        }),
    );
    Value::Object(map)
}

/// Walk a parsed YTM/yt JSON tree collecting objects whose single key matches
/// `needle`. YTM nests renderers a few levels deep; enumerating every path by
/// hand breaks with every client version bump, so we look for the renderer key
/// itself at any depth.
fn collect_named(node: &Value, needle: &str) -> Vec<Value> {
    let mut out = Vec::new();
    fn walk(node: &Value, needle: &str, out: &mut Vec<Value>) {
        match node {
            Value::Object(map) => {
                if let Some(inner) = map.get(needle) {
                    if let Some(obj) = inner.as_object() {
                        out.push(Value::Object(obj.clone()));
                    }
                }
                for (_, value) in map {
                    walk(value, needle, out);
                }
            }
            Value::Array(items) => {
                for value in items {
                    walk(value, needle, out);
                }
            }
            _ => {}
        }
    }
    walk(node, needle, &mut out);
    out
}

fn first_text(node: &Value) -> String {
    match node {
        Value::String(s) => s.clone(),
        Value::Object(map) => map
            .get("runs")
            .and_then(|r| r.as_array())
            .map(|runs| {
                runs.iter()
                    .filter_map(|run| run.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn duration_from(text: &str) -> u64 {
    let parsed = ytm::parse_duration(text);
    if parsed > 0 { parsed } else { 180 }
}

fn thumb_from(node: &Value, default_vid: Option<&str>) -> String {
    // simple renderer-shaped thumbnail lists nest under `thumbnails`
    fn find_thumbs(node: &Value) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(node: &Value, out: &mut Vec<String>) {
            match node {
                Value::Object(map) => {
                    if let Some(items) = map.get("thumbnails").and_then(|v| v.as_array()) {
                        for item in items {
                            if let Some(url) = item.get("url").and_then(|v| v.as_str()) {
                                out.push(url.to_string());
                            }
                        }
                    }
                    for (_, v) in map {
                        walk(v, out);
                    }
                }
                Value::Array(items) => {
                    for v in items {
                        walk(v, out);
                    }
                }
                _ => {}
            }
        }
        walk(node, &mut out);
        out
    }
    find_thumbs(node)
        .into_iter()
        .max_by_key(|u| {
            u.len()
        })
        .unwrap_or_else(|| default_vid.map(|v| format!("https://i.ytimg.com/vi/{v}/hqdefault.jpg")).unwrap_or_default())
}

fn simple_hash(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in input.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn format_listeners(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}K", n / 1_000)
    } else {
        n.to_string()
    }
}

fn synthetic_listeners(seed: &str, index: usize) -> u64 {
    let base = 180_000u64 + (simple_hash(seed) % 3_020_000);
    let decay = 1.0 - (index as f64 * 0.018).min(0.72);
    ((base as f64 * decay) as u64).max(35_000)
}

fn normalized_key(title: &str, artist: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    format!("{}::{}", clean(artist), clean(title))
}

/// Strip the decorators that make a remix of the same song look like its own
/// track. `taste::core_title` is the single implementation used by the queue,
/// the taste profile and the vibe filter, so client side and server side agree.
fn family_key(title: &str, artist: &str) -> String {
    normalized_key(&taste::core_title(title), &normalize_artist(artist))
}

fn core_key(title: &str, artist: &str) -> String {
    family_key(title, artist)
}

/// Older clients send `artist::title` keys built by the renderer. Normalize
/// both sides through the same core-title function so "song (slowed)" and
/// "song" collide even when the client key kept the decorator.
fn avoid_core_key(key: &str) -> String {
    let (artist, title) = key.split_once("::").unwrap_or(("", key));
    core_key(title, artist)
}

fn is_genre_tag(tag: &str) -> bool {
    !matches!(
        tag,
        "sad"
            | "emotional"
            | "heartbreak"
            | "lonely"
            | "happy"
            | "chill"
            | "dark"
            | "aggressive"
            | "hype"
            | "dreamy"
            | "nostalgic"
            | "romantic"
            | "peaceful"
            | "atmospheric"
    )
}

fn push_tag(map: &mut HashMap<String, f64>, raw: &str, weight: f64) {
    if let Some(tag) = taste::canonical_tag(raw) {
        *map.entry(tag).or_insert(0.0) += weight;
    }
}

fn absorb_tag_list(value: Option<&Value>, map: &mut HashMap<String, f64>, base: f64) {
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return;
    };
    for item in items {
        if let Some(name) = item.as_str() {
            push_tag(map, name, base);
        } else if let Some(obj) = item.as_object() {
            let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let weight = obj.get("weight").and_then(|v| v.as_f64()).unwrap_or(base);
            push_tag(map, name, weight.max(base * 0.25));
        }
    }
}

fn absorb_track_tags(tags: &[String], genres: &mut HashMap<String, f64>, moods: &mut HashMap<String, f64>, base: f64) {
    for tag in tags {
        let map = if is_genre_tag(tag) { &mut *genres } else { &mut *moods };
        *map.entry(tag.clone()).or_insert(0.0) += base;
    }
}

fn artist_is_avoided(avoid_artists: &[String], artist: &str) -> bool {
    let candidate = normalize_artist(artist);
    if candidate.is_empty() {
        return false;
    }
    avoid_artists.iter().any(|blocked| {
        let blocked = normalize_artist(blocked);
        if blocked.is_empty() {
            return false;
        }
        candidate == blocked
            || candidate.starts_with(&format!("{blocked} "))
            || candidate.contains(&format!(" {blocked} "))
            || blocked.starts_with(&format!("{candidate} "))
    })
}

struct AvoidSet {
    ids: HashSet<String>,
    keys: HashSet<String>,
    cores: HashSet<String>,
    titles: HashSet<String>,
    artists: Vec<String>,
}

impl AvoidSet {
    fn from_body(body: &Value) -> Self {
        let avoid = body.get("avoid").cloned().unwrap_or(Value::Null);
        let mut ids: HashSet<String> = avoid
            .get("ids")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect();
        let mut keys: HashSet<String> = avoid
            .get("keys")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect();
        let artists: Vec<String> = avoid
            .get("artists")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        // A recommendation must never hand back a track the user already
        // played. The renderer sends the last 30 history rows, but older
        // clients only send skips, so fold history in server side as well.
        if let Some(history) = body.get("history").and_then(|v| v.as_array()) {
            for track in history {
                if let Some(id) = track.get("id").and_then(|v| v.as_str()) {
                    if !id.trim().is_empty() {
                        ids.insert(id.trim().to_string());
                    }
                }
                let artist = track.get("artist").and_then(|v| v.as_str()).unwrap_or("");
                let title = track.get("title").and_then(|v| v.as_str()).unwrap_or("");
                if !artist.trim().is_empty() && !title.trim().is_empty() {
                    keys.insert(format!("{}::{}", normalize_artist(artist), taste::core_title(title)));
                }
            }
        }

        // The anchor is the song currently playing; its remix/tekk uploads
        // must never be queued as "new" tracks either.
        if let Some(anchor) = body.get("anchor") {
            let id = anchor.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let artist = anchor.get("artist").and_then(|v| v.as_str()).unwrap_or("");
            let title = anchor.get("title").and_then(|v| v.as_str()).unwrap_or("");
            if !id.trim().is_empty() {
                ids.insert(id.trim().to_string());
            }
            if !artist.trim().is_empty() && !title.trim().is_empty() {
                keys.insert(format!("{}::{}", normalize_artist(artist), taste::core_title(title)));
            }
        }

        let mut cores = HashSet::new();
        let mut titles = HashSet::new();
        for key in &keys {
            let core = avoid_core_key(key);
            if let Some((_, title)) = core.split_once("::") {
                if title.len() >= 8 {
                    titles.insert(title.to_string());
                }
            }
            cores.insert(core);
        }
        Self { ids, keys, cores, titles, artists }
    }

    fn blocks(&self, id: &str, title: &str, artist: &str) -> bool {
        if !id.is_empty() && self.ids.contains(id) {
            return true;
        }
        let key = normalized_key(title, artist);
        if self.keys.contains(&key) {
            return true;
        }
        let fam = family_key(title, artist);
        if self.keys.contains(&fam) || self.cores.contains(&fam) {
            return true;
        }
        if artist_is_avoided(&self.artists, artist) {
            return true;
        }
        let core = core_key(title, artist);
        if self.cores.contains(&core) {
            return true;
        }
        let cand_core = core.clone();
        let cand_title = cand_core.split_once("::").map(|(_, t)| t).unwrap_or("");
        if cand_title.len() >= 8 && self.titles.contains(cand_title) {
            return true;
        }
        // Fuzzy family check for keys built by the old renderer: same artist
        // and one core title contains the other, but only when the shared
        // title is long enough to be meaningful.
        let cand_artist = normalize_artist(artist);
        if cand_title.len() >= 5 {
            for avoided in &self.cores {
                let (a, t) = avoided.split_once("::").unwrap_or(("", ""));
                if a != cand_artist || t.len() < 5 {
                    continue;
                }
                if cand_title.contains(t) || t.contains(cand_title) {
                    return true;
                }
            }
        }
        false
    }
}

fn with_listener_fields(track: &mut serde_json::Map<String, Value>, index: usize) {
    let key = format!(
        "{}:{}",
        track.get("artist").and_then(|v| v.as_str()).unwrap_or(""),
        track.get("title").and_then(|v| v.as_str()).unwrap_or("")
    );
    let listeners = track
        .get("rank")
        .and_then(|v| v.as_u64())
        .filter(|r| *r > 50_000)
        .unwrap_or_else(|| synthetic_listeners(&key, index));
    track.insert("listeners".to_string(), json!(listeners));
    track.insert("listenersText".to_string(), json!(format_listeners(listeners)));
    if track.get("cover").is_none() {
        if let Some(t) = track.get("thumbnail").cloned() {
            track.insert("cover".to_string(), t);
        }
    }
    if track.get("thumbnail").is_none() {
        if let Some(t) = track.get("cover").cloned() {
            track.insert("thumbnail".to_string(), t);
        }
    }
}

fn json_track(
    id: String,
    title: String,
    artist: String,
    thumbnail: String,
    duration: u64,
    platform: &str,
    source: &str,
    is_explicit: Option<bool>,
) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), json!(id));
    map.insert("title".to_string(), json!(title));
    map.insert("artist".to_string(), json!(artist));
    map.insert("thumbnail".to_string(), json!(thumbnail.clone()));
    map.insert("cover".to_string(), json!(thumbnail));
    map.insert("duration".to_string(), json!(duration));
    map.insert("platform".to_string(), json!(platform));
    map.insert("source".to_string(), json!(source));
    map.insert("url".to_string(), Value::String(String::new()));
    if let Some(explicit) = is_explicit {
        map.insert("isExplicit".to_string(), json!(explicit));
    }
    Value::Object(map)
}

fn flex_col_text(renderer: &Value, column: usize) -> String {
    renderer
        .get("flexColumns")
        .and_then(|v| v.as_array())
        .and_then(|items| items.get(column))
        .and_then(|col| col.get("musicResponsiveListItemFlexColumnRenderer"))
        .and_then(|inner| inner.get("text"))
        .map(first_text)
        .unwrap_or_default()
}

fn fixed_col_text(renderer: &Value, column: usize) -> String {
    renderer
        .get("fixedColumns")
        .and_then(|v| v.as_array())
        .and_then(|items| items.get(column))
        .and_then(|col| col.get("musicResponsiveListItemfixedColumnRenderer"))
        .and_then(|inner| inner.get("text"))
        .map(first_text)
        .unwrap_or_default()
}

fn explicit_badge(renderer: &Value) -> bool {
    renderer
        .get("badges")
        .and_then(|b| b.as_array())
        .map(|badges| {
            badges.iter().any(|badge| {
                badge
                    .get("text")
                    .map(first_text)
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains("explicit")
            })
        })
        .unwrap_or(false)
}

fn watch_video_id(renderer: &Value) -> Option<String> {
    renderer
        .get("navigationEndpoint")
        .and_then(|n| n.get("watchEndpoint"))
        .and_then(|w| w.get("videoId"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            renderer
                .get("playlistItemData")
                .and_then(|p| p.get("videoId"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .or_else(|| {
            renderer
                .get("videoId")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
}

/// RDAMVM/queue "next" rows.
fn parse_panel_row(renderer: &Value) -> Option<(String, String, String, String, u64, bool)> {
    let video_id = watch_video_id(renderer)?;
    let title = renderer.get("title").map(first_text)?.trim().to_string();
    if title.is_empty() {
        return None;
    }
    let byline = renderer
        .get("shortBylineText")
        .map(first_text)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| renderer.get("longBylineText").map(first_text))
        .map(|s| {
            s.split(" • ")
                .next()
                .unwrap_or(&s)
                .trim()
                .trim_end_matches(" - Topic")
                .to_string()
        })?;
    if byline.is_empty() {
        return None;
    }
    let duration = duration_from(&renderer.get("lengthText").map(first_text).unwrap_or_default());
    let thumb = thumb_from(renderer, Some(&video_id));
    Some((video_id, title, byline, thumb, duration, explicit_badge(renderer)))
}

/// Chart/playlist rows (musicResponsiveListItemRenderer). Rows without a
/// videoId (e.g. "Top artists" rows) are skipped.
fn parse_list_row(renderer: &Value) -> Option<(String, String, String, String, u64, bool)> {
    let video_id = watch_video_id(renderer)?;
    let title = flex_col_text(renderer, 0).trim().to_string();
    let artist = flex_col_text(renderer, 1);
    let artist = if artist.is_empty() {
        let short = renderer
            .get("shortBylineText")
            .map(first_text)
            .unwrap_or_default();
        if short.is_empty() {
            return None;
        }
        short
    } else {
        artist
    };
    let artist = artist.trim().trim_end_matches(" - Topic").to_string();
    if title.is_empty() || artist.is_empty() {
        return None;
    }
    let duration = duration_from(&fixed_col_text(renderer, 0));
    let thumb = thumb_from(renderer, Some(&video_id));
    Some((video_id, title, artist, thumb, duration, explicit_badge(renderer)))
}
async fn ytm_post(state: &Shared, path: &str, body: Value) -> Result<Value, String> {
    let url = format!("{YTM_API_BASE}/{path}?prettyPrint=false");
    let client = state.http();
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ytm {path}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("ytm {path}: HTTP {}", response.status().as_u16()));
    }
    response.json::<Value>().await.map_err(|e| format!("ytm {path}: {e}"))
}

/// YTM RDAMVM mix for a video id. Falls back to empty vec â€” caller degrades gracefully.
async fn ytm_mix_tracks(state: &Shared, seed_vid: &str, limit: usize) -> Vec<Value> {
    let body = ytm_body(
        "WEB_REMIX",
        json!({ "playlistId": format!("RDAMVM{seed_vid}") }),
    );
    let Ok(json) = ytm_post(state, "next", body).await else {
        return Vec::new();
    };
    let mut tracks = Vec::new();
    for renderer in collect_named(&json, "playlistPanelVideoRenderer").into_iter().take(limit + 1) {
        if let Some((id, title, artist, thumb, duration, explicit)) = parse_panel_row(&renderer) {
            if id == seed_vid {
                continue;
            }
            tracks.push(json_track(id, title, artist, thumb, duration, "youtube", "ytmix", Some(explicit)));
            if tracks.len() >= limit {
                break;
            }
        }
    }
    tracks
}

fn chart_score(combined: &str) -> u32 {
    if combined.contains("top 100") || combined.contains("топ 100") {
        30
    } else if combined.contains("trending") || combined.contains("тренд") {
        20
    } else if combined.contains("top 50") || combined.contains("топ 50") {
        10
    } else if combined.contains("top") || combined.contains("топ") || combined.contains("chart") {
        5
    } else {
        0
    }
}

/// YTM charts browse. `country` is a lowercase ISO code or "global".
///
/// The charts page contains no raw track rows — it contains "chart cards"
/// (musicTwoRowItemRenderer) pointing at YouTube Chart playlists. Pick the
/// best card, browse that playlist, and parse its track rows.
async fn ytm_chart_tracks(state: &Shared, country: &str, limit: usize) -> Vec<Value> {
    let gl = match country {
        "global" => "ZZ",
        other => other,
    };
    let body = ytm_body(
        "WEB_REMIX",
        json!({
            "browseId": "FEmusic_charts",
            "formData": { "selectedValues": [gl.to_uppercase()] }
        }),
    );
    let Ok(page) = ytm_post(state, "browse", body).await else {
        return Vec::new();
    };

    // Collect chart cards and rank them. Not every country phrases its chart
    // row with the word "chart", so anything that carries a playlist id is a
    // candidate and chart_score sorts them.
    let mut cards: Vec<(u32, String)> = Vec::new();
    for card in collect_named(&page, "musicTwoRowItemRenderer") {
        let title = card.get("title").map(first_text).unwrap_or_default().to_lowercase();
        let subtitle = card.get("subtitle").map(first_text).unwrap_or_default().to_lowercase();
        let combined = format!("{title} {subtitle}");
        let playlist = card
            .get("navigationEndpoint")
            .and_then(|n| n.get("browseEndpoint"))
            .and_then(|b| b.get("browseId"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                card.get("navigationEndpoint")
                    .and_then(|n| n.get("watchPlaylistEndpoint"))
                    .and_then(|w| w.get("playlistId"))
                    .and_then(|v| v.as_str())
            })
            .map(String::from);
        let Some(playlist) = playlist else { continue };
        cards.push((chart_score(&combined), playlist));
    }
    cards.sort_by(|a, b| b.0.cmp(&a.0));

    let mut tracks = Vec::new();
    for (_, playlist_id) in cards.into_iter().take(4) {
        let body = ytm_body("WEB_REMIX", json!({ "browseId": playlist_id }));
        let Ok(playlist_page) = ytm_post(state, "browse", body).await else {
            continue;
        };
        tracks.clear();
        for renderer in collect_named(&playlist_page, "musicResponsiveListItemRenderer") {
            let Some((id, title, artist, thumb, duration, explicit)) =
                parse_list_row(&renderer)
            else {
                continue;
            };
            tracks.push(json_track(id, title, artist, thumb, duration, "ytmusic", "ytmusic", Some(explicit)));
            if tracks.len() >= limit {
                break;
            }
        }
        if !tracks.is_empty() {
            return tracks;
        }
    }
    Vec::new()
}

// --------------------------------------------------------------- vibe

fn vibe_profile(history: &[Value], favorites: &[Value], client_profile: &Value) -> Value {
    let mut weights: HashMap<String, f64> = HashMap::new();
    let mut ids = Vec::new();
    let mut add = |track: &Value, weight: f64| {
        let artist = track
            .get("artist")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if artist.is_empty() {
            return;
        }
        let e = weights.entry(normalize_artist(&artist)).or_insert(0.0);
        *e += weight;
        ids.push(artist.clone());
    };
    for (index, track) in favorites.iter().enumerate() {
        let decay = 0.5_f64.powf(index as f64 * 0.12);
        add(track, 1.0 + decay);
    }
    for (index, track) in history.iter().enumerate() {
        let decay = 0.94_f64.powi(index as i32);
        add(track, decay);
    }
    if let Some(list) = client_profile.get("topArtists").and_then(|v| v.as_array()) {
        for artist in list {
            if let Some(name) = artist.as_str() {
                let e = weights.entry(normalize_artist(name)).or_insert(0.0);
                *e += 4.0;
            }
        }
    }
    let mut top: Vec<(String, f64)> = weights.into_iter().collect();
    top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top_artists: Vec<String> = top.into_iter().take(5).map(|(name, _)| name).collect();
    let text = history.iter().chain(favorites.iter()).filter_map(|t| t.get("title").and_then(|v| v.as_str())).collect::<Vec<_>>().join(" ");
    let cyrillic = text.chars().filter(|c| ('\u{0400}'..='\u{04FF}').contains(c)).count();
    let dominant_script = if !text.is_empty() && cyrillic * 100 > text.chars().count() * 35 {
        "Cyrillic"
    } else {
        "Latin"
    };
    json!({
        "topArtists": top_artists,
        "dominantScript": dominant_script,
        "typicalDuration": Value::Null,
        "trackCount": history.len() + favorites.len(),
        "seedArtists": top_artists.clone(),
    })
}

fn normalize_artist(name: &str) -> String {
    taste::normalize_artist(name)
}

pub async fn vibe(state: &Shared, body: &Value) -> Value {
    let refresh = body.get("refresh").and_then(|v| v.as_bool()).unwrap_or(false);
    let anchor_key = body
        .get("anchor")
        .and_then(|a| {
            let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let artist = a.get("artist").and_then(|v| v.as_str()).unwrap_or("");
            let title = a.get("title").and_then(|v| v.as_str()).unwrap_or("");
            Some(format!("{id}:{artist}:{title}"))
        })
        .unwrap_or_default();
    let avoid_fingerprint = body
        .get("avoid")
        .map(|v| format!("{:x}", simple_hash(&v.to_string())))
        .unwrap_or_else(|| "none".to_string());
    let cache_key = format!(
        "disc:vibe:{}:{}:{}",
        simple_hash(&format!(
            "{anchor_key}|{}|{}",
            body.get("mode").and_then(|v| v.as_str()).unwrap_or("foryou"),
            body.get("seeds")
                .or_else(|| body.get("favorites"))
                .map(|v| v.to_string())
                .unwrap_or_else(|| "empty".to_string())
        )),
        avoid_fingerprint,
        body.get("limit").and_then(|v| v.as_u64()).unwrap_or(10)
    );
    if !refresh {
        if let Some(blob) = cache_get(&state.stream_cache, &cache_key, Duration::from_secs(20 * 60)) {
            return json!({
                "success": true,
                "profile": blob.get("profile").cloned().unwrap_or(json!({})),
                "vibe": blob.get("vibe").cloned().unwrap_or(json!({})),
                "tracks": blob.get("tracks").cloned().unwrap_or(json!([])),
            });
        }
    }

    let limit = body
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10)
        .clamp(4, 16) as usize;
    let history = body.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let favorites = body.get("favorites").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let client_profile = body.get("clientProfile").cloned().unwrap_or(Value::Null);
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("foryou");

    let profile = vibe_profile(&history, &favorites, &client_profile);
    let top_artists: Vec<String> = profile
        .get("topArtists")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    // The genre/mood the queue must stay inside. The taste endpoint already
    // resolved Last.fm tags; for radio the anchor track is the strongest
    // signal, so its tags are fetched once (cached 24 h) and dominate.
    let mut vibe_genres: HashMap<String, f64> = HashMap::new();
    let mut vibe_moods: HashMap<String, f64> = HashMap::new();
    absorb_tag_list(client_profile.get("genres"), &mut vibe_genres, 2.0);
    absorb_tag_list(client_profile.get("moods"), &mut vibe_moods, 2.0);
    if mode != "radio" {
        absorb_tag_list(client_profile.get("dailyGenres"), &mut vibe_genres, 3.0);
        absorb_tag_list(client_profile.get("dailyMoods"), &mut vibe_moods, 3.0);
    }

    let anchor = body.get("anchor").filter(|v| v.is_object()).cloned();
    if let Some(anchor) = anchor.as_ref() {
        let artist = anchor.get("artist").and_then(|v| v.as_str()).unwrap_or("").trim();
        let title = anchor.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
        if !artist.is_empty() {
            let mut tags = taste::track_tags(state, artist, title).await;
            if tags.is_empty() {
                tags = taste::artist_tags(state, artist).await;
            }
            for tag in taste::classify_text(title, artist) {
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
            absorb_track_tags(&tags, &mut vibe_genres, &mut vibe_moods, 4.0);
        }
    }

    // Seed selection: the anchor track first, then explicit seeds. The client
    // sends the currently playing song for radio refills, so the queue keeps
    // the genre of the song the user is listening to instead of drifting into
    // the all-time favourites mix.
    struct Seed {
        artist: String,
        title: String,
        id: String,
        weight: f64,
        anchor: bool,
    }
    let mut seeds: Vec<Seed> = Vec::new();
    let mut seen_seed_keys: HashSet<String> = HashSet::new();
    if let Some(anchor) = anchor.as_ref() {
        let artist = anchor.get("artist").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        let title = anchor.get("title").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
        if !artist.is_empty() {
            let id = anchor.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            seen_seed_keys.insert(core_key(&title, &artist));
            seeds.push(Seed { artist, title, id, weight: 10.0, anchor: true });
        }
    }
    if let Some(list) = body.get("seeds").and_then(|v| v.as_array()) {
        for seed in list {
            let artist = seed.get("artist").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            if artist.is_empty() {
                continue;
            }
            let title = seed.get("title").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            let key = core_key(&title, &artist);
            if !seen_seed_keys.insert(key) {
                continue;
            }
            let platform = seed
                .get("platform")
                .or_else(|| seed.get("source"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_lowercase();
            let weight = seed.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0);
            let mut id = seed.get("id").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            let usable = !id.is_empty()
                && platform != "soundcloud"
                && id.len() >= 8
                && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !usable {
                id = String::new();
            }
            seeds.push(Seed { artist, title, id, weight, anchor: false });
        }
    }
    if seeds.is_empty() {
        // fall back to top artists as seeds
        seeds = top_artists
            .iter()
            .take(8)
            .map(|a| Seed {
                artist: a.clone(),
                title: String::new(),
                id: String::new(),
                weight: 2.0,
                anchor: false,
            })
            .collect();
    }

    // If nothing above produced tags (older client payloads), classify the
    // seed text so a breakcore or sad title still steers the mix.
    if vibe_genres.is_empty() && vibe_moods.is_empty() {
        for seed in &seeds {
            absorb_track_tags(
                &taste::classify_text(&seed.title, &seed.artist),
                &mut vibe_genres,
                &mut vibe_moods,
                1.0,
            );
        }
    }

    // Cluster the seeds: one anchor plus songs that share the vibe. Mixing a
    // breakcore anchor with a pop favourite is what made the queue drift into
    // a different genre. Seeds with an explicit conflicting genre are dropped.
    for seed in &mut seeds {
        let tags = taste::classify_text(&seed.title, &seed.artist);
        let mut overlap = 0.0;
        let mut has_genre = false;
        let mut genre_conflict = false;
        for tag in &tags {
            if is_genre_tag(tag) {
                has_genre = true;
                if let Some(weight) = vibe_genres.get(tag) {
                    overlap += weight;
                }
            } else if let Some(weight) = vibe_moods.get(tag) {
                overlap += weight * 0.5;
            }
        }
        if has_genre && !vibe_genres.is_empty() && overlap == 0.0 {
            genre_conflict = true;
        }
        seed.weight += overlap * 0.5;
        if seed.anchor {
            seed.weight += 25.0;
        } else if genre_conflict {
            seed.weight = -1.0;
        }
    }
    seeds.retain(|seed| seed.weight > 0.0);
    seeds.sort_by(|a, b| b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal));
    seeds.truncate(if mode == "radio" { 3 } else { 5 });

    // Resolve seed video ids. Use the track's own id when we have one, else
    // search the best matching song by artist and title.
    let seed_futures: Vec<_> = seeds
        .iter()
        .take(seeds.len())
        .map(|seed| {
            let client = state.http();
            let artist = seed.artist.clone();
            let title = seed.title.clone();
            let id = seed.id.clone();
            let weight = seed.weight;
            async move {
                if !id.is_empty() {
                    return (Some(id), weight);
                }
                let query = if title.is_empty() {
                    artist.clone()
                } else {
                    format!("{artist} {title}")
                };
                let tracks = ytm::search(&client, &query, 3).await.unwrap_or_default();
                let id = if !title.is_empty() {
                    tracks
                        .iter()
                        .find(|t| {
                            t.get("title")
                                .and_then(|v| v.as_str())
                                .map(|tt| {
                                    let tt = tt.to_lowercase();
                                    let title = title.to_lowercase();
                                    tt.contains(&title) || title.contains(&tt)
                                })
                                .unwrap_or(false)
                        })
                        .or_else(|| tracks.first())
                        .and_then(|t| t.get("id").and_then(|v| v.as_str()).map(String::from))
                } else {
                    tracks
                        .into_iter()
                        .next()
                        .and_then(|t| t.get("id").and_then(|v| v.as_str()).map(String::from))
                };
                (id, weight)
            }
        })
        .collect();
    let seed_ids: Vec<(String, f64)> = futures_util::future::join_all(seed_futures)
        .await
        .into_iter()
        .filter_map(|(id, weight)| id.map(|id| (id, weight)))
        .collect();

    let mut candidates: Vec<(Value, f64)> = Vec::new();
    let mix_futures: Vec<_> = seed_ids
        .iter()
        .map(|(id, _weight)| {
            let st = state.clone();
            let id = id.clone();
            async move { ytm_mix_tracks(&st, &id, 24).await }
        })
        .collect();
    for ((_id, weight), tracks) in seed_ids.iter().zip(futures_util::future::join_all(mix_futures).await) {
        for track in tracks {
            candidates.push((track, weight * 8.0));
        }
    }

    // Deezer related tracks for diversity — seeds and their related artists in flight.
    let dz_futures: Vec<_> = seeds
        .iter()
        .map(|seed| {
            let st = state.clone();
            let artist = seed.artist.clone();
            let weight = seed.weight;
            async move {
                let mut out: Vec<(Value, f64)> = Vec::new();
                let encoded = util::percent_encode(&artist);
                let url = format!("https://api.deezer.com/search/artist?q={encoded}&limit=1");
                let Ok(resp) = st.http().get(&url).send().await else { return out };
                let Ok(json) = resp.json::<Value>().await else { return out };
                let Some(seed_id) = json
                    .get("data")
                    .and_then(|d| d.as_array())
                    .and_then(|a| a.first())
                    .and_then(|a| a.get("id"))
                    .and_then(|v| v.as_u64())
                else { return out };
                let rel = format!("https://api.deezer.com/artist/{seed_id}/related?limit=6");
                let Ok(resp) = st.http().get(&rel).send().await else { return out };
                let Ok(json) = resp.json::<Value>().await else { return out };
                let top_futures: Vec<_> = json
                    .get("data")
                    .and_then(|d| d.as_array())
                    .into_iter()
                    .flatten()
                    .take(3)
                    .map(|related| {
                        let st = st.clone();
                        let name = related.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let rid = related.get("id").and_then(|v| v.as_u64());
                        let weight = weight;
                        async move {
                            let mut out = Vec::new();
                            let Some(rid) = rid else { return out };
                            let top = format!("https://api.deezer.com/artist/{rid}/top?limit=5");
                            let Ok(resp) = st.http().get(&top).send().await else { return out };
                            let Ok(j) = resp.json::<Value>().await else { return out };
                            let Some(items) = j.get("data").and_then(|d| d.as_array()) else { return out };
                            for item in items {
                                let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                                let duration = item.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
                                let cover = item
                                    .get("album")
                                    .and_then(|a| a.get("cover_big").or_else(|| a.get("cover_medium")))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if title.is_empty() || name.is_empty() {
                                    continue;
                                }
                                let explicit = item.get("explicit_lyrics").and_then(|v| v.as_bool()).unwrap_or(false);
                                out.push((json_track(
                                    format!("dz-{}", item.get("id").and_then(|v| v.as_str()).unwrap_or("")),
                                    title.to_string(),
                                    name.clone(),
                                    cover.to_string(),
                                    duration,
                                    "youtube",
                                    "deezer",
                                    Some(explicit),
                                ), weight * 6.0));
                            }
                            out
                        }
                    })
                    .collect();
                for tracks in futures_util::future::join_all(top_futures).await {
                    out.extend(tracks);
                }
                out
            }
        })
        .collect();
    for tracks in futures_util::future::join_all(dz_futures).await {
        candidates.extend(tracks);
    }

    // Deduplicate + rank. Every candidate is checked against the anchor, the
    // queue, history, skips and blocked artists. Genre/mood conflicts from
    // generic providers are dropped; YTM mix rows are only downranked because
    // the mix itself is already genre-derived and titles can be misleading.
    let avoid = AvoidSet::from_body(body);
    let mut scored: HashMap<String, (Value, f64)> = HashMap::new();
    for (track, score) in candidates {
        let id = track.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let title = track.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let artist = track.get("artist").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if avoid.blocks(&id, &title, &artist) {
            continue;
        }
        let duration = track.get("duration").and_then(|v| v.as_u64()).unwrap_or(0);
        if duration == 0 || duration > 720 {
            continue;
        }

        let tags = taste::classify_text(&title, &artist);
        let mut vibe_bonus = 0.0;
        let mut genre_match = false;
        let mut genre_conflict = false;
        for tag in &tags {
            if is_genre_tag(tag) {
                if let Some(weight) = vibe_genres.get(tag) {
                    vibe_bonus += 3.0 + weight.min(8.0);
                    genre_match = true;
                } else if !vibe_genres.is_empty() {
                    genre_conflict = true;
                }
            } else if let Some(weight) = vibe_moods.get(tag) {
                vibe_bonus += 2.0 + weight.min(6.0) * 0.5;
            }
        }
        let source = track.get("source").and_then(|v| v.as_str()).unwrap_or("");
        let from_mix = source == "ytmix" || source == "ytmusic" || source.is_empty();
        if genre_conflict && !from_mix {
            continue;
        }
        if genre_conflict {
            vibe_bonus -= 12.0;
        }
        let mut bonus = score + vibe_bonus;
        if genre_match {
            bonus += 6.0;
        }
        let artist_lower = artist.to_lowercase();
        for top in &top_artists {
            if artist_lower.contains(&top.to_lowercase()) {
                bonus += 2.0;
            }
        }
        let key = core_key(&title, &artist);
        let entry = scored.entry(key).or_insert_with(|| (track.clone(), f64::NEG_INFINITY));
        if bonus > entry.1 {
            let mut track = track;
            if let Some(map) = track.as_object_mut() {
                map.insert("vibeTags".to_string(), json!(tags));
            }
            entry.0 = track;
            entry.1 = bonus;
        }
    }

    let mut picks: Vec<(Value, f64)> = scored.into_values().collect();
    picks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let anchor_artist = anchor
        .as_ref()
        .and_then(|a| a.get("artist"))
        .and_then(|v| v.as_str())
        .map(normalize_artist)
        .unwrap_or_default();
    let radio = mode == "radio";
    let mut per_artist: HashMap<String, u32> = HashMap::new();
    let mut per_family: HashSet<String> = HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    for (mut track, score) in picks {
        let artist = track.get("artist").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let title = track.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let norm_artist = normalize_artist(&artist);
        let cap = if radio && norm_artist != anchor_artist { 1 } else { 2 };
        let count = per_artist.entry(norm_artist).or_insert(0);
        if *count >= cap {
            continue;
        }
        let fam = core_key(&title, &artist);
        if !per_family.insert(fam) {
            continue;
        }
        *count += 1;
        let search_query = format!("{artist} - {title}");
        if let Some(map) = track.as_object_mut() {
            map.insert("score".to_string(), json!(score));
            map.insert("searchQuery".to_string(), json!(search_query));
            with_listener_fields(map, out.len());
        }
        out.push(track);
        if out.len() >= limit {
            break;
        }
    }

    // Fallback: stay inside the vibe instead of falling back to generic pop
    // charts. Radio searches the anchor artist/genre; For You uses the top
    // artists. Every fallback row goes through the same avoid/family gate.
    if out.len() < limit {
        let mut queries: Vec<String> = Vec::new();
        if let Some(anchor) = anchor.as_ref() {
            let artist = anchor.get("artist").and_then(|v| v.as_str()).unwrap_or("");
            let title = anchor.get("title").and_then(|v| v.as_str()).unwrap_or("");
            if !artist.is_empty() {
                queries.push(format!("{artist} {title}"));
                queries.push(format!("{artist} radio"));
            }
        }
        if let Some((genre, _)) = vibe_genres.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal)) {
            queries.push(format!("{genre} mix"));
        }
        queries.extend(top_artists.iter().take(4).cloned());
        queries.dedup();

        for query in queries {
            if out.len() >= limit {
                break;
            }
            if let Ok(tracks) = ytm::search(&state.http(), &query, 8).await {
                for track in tracks {
                    if out.len() >= limit {
                        break;
                    }
                    let id = track.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let artist = track.get("artist").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let title = track.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if avoid.blocks(&id, &title, &artist) {
                        continue;
                    }
                    let fam = core_key(&title, &artist);
                    if per_family.contains(&fam) {
                        continue;
                    }
                    let norm_artist = normalize_artist(&artist);
                    let count = per_artist.get(&norm_artist).copied().unwrap_or(0);
                    if count >= if radio && norm_artist != anchor_artist { 1 } else { 2 } {
                        continue;
                    }
                    let tags = taste::classify_text(&title, &artist);
                    let mismatched = tags.iter().any(|tag| is_genre_tag(tag) && !vibe_genres.is_empty() && !vibe_genres.contains_key(tag));
                    if mismatched {
                        continue;
                    }
                    per_family.insert(fam);
                    per_artist.insert(norm_artist, count + 1);
                    let mut track = track;
                    if let Some(map) = track.as_object_mut() {
                        map.insert("source".to_string(), json!("ytsearch"));
                        map.insert("vibeTags".to_string(), json!(tags));
                        map.insert("score".to_string(), json!(3.0));
                        map.insert("searchQuery".to_string(), json!(query));
                        with_listener_fields(map, out.len());
                    }
                    out.push(track);
                }
            }
        }
    }

    let vibe_payload = json!({
        "genres": vibe_genres.iter().map(|(k, v)| json!({ "name": k, "weight": v })).collect::<Vec<_>>(),
        "moods": vibe_moods.iter().map(|(k, v)| json!({ "name": k, "weight": v })).collect::<Vec<_>>(),
        "mode": mode,
    });
    cache_put(
        &state.stream_cache,
        cache_key,
        json!({ "profile": profile, "vibe": vibe_payload, "tracks": out }),
        256,
        Duration::from_secs(20 * 60),
    );
    json!({ "success": true, "profile": profile, "vibe": vibe_payload, "tracks": out })
}

// --------------------------------------------------------------- trending / chart / hot

pub async fn trending(state: &Shared, query: &HashMap<String, String>) -> Value {
    let region = query
        .get("region")
        .map(|r| r.trim().to_lowercase())
        .filter(|r| (r.chars().all(|c| c.is_ascii_lowercase()) && r.len() == 2) || r == "global")
        .unwrap_or_else(|| "us".to_string());

    // Real YouTube Music charts for the region, with iTunes and Last.fm as
    // network fallbacks. This replaced the old hardcoded scene lists.
    let chart_query = HashMap::from([(String::from("country"), region)]);
    let chart = top_chart(state, &chart_query).await;
    let tracks = chart.get("tracks").cloned().unwrap_or_else(|| Value::Array(vec![]));
    json!({ "success": true, "tracks": tracks })
}

pub async fn top_chart(state: &Shared, query: &HashMap<String, String>) -> Value {
    let country = query
        .get("country")
        .map(|c| c.trim().to_lowercase())
        .filter(|c| c == "global" || (c.chars().all(|ch| ch.is_ascii_lowercase()) && c.len() == 2))
        .unwrap_or_else(|| "us".to_string());

    let cache_key = format!("disc:chart:{country}");
    if let Some(cached) = cache_get(&state.stream_cache, &cache_key, Duration::from_secs(30 * 60)) {
        return json!({ "success": true, "country": country, "tracks": cached });
    }

    let mut tracks: Vec<Value> = ytm_chart_tracks(state, &country, 50).await;

    if tracks.len() < 8 && country != "global" {
        let itunes_url = format!("https://itunes.apple.com/{}/rss/topsongs/limit=40/json", country);
        if let Ok(resp) = state.http().get(&itunes_url).send().await {
            if let Ok(json) = resp.json::<Value>().await {
                if let Some(entries) = json
                    .get("feed")
                    .and_then(|f| f.get("entry"))
                    .and_then(|e| e.as_array())
                {
                    for entry in entries.iter().take(40) {
                        let title = entry
                            .get("im:name")
                            .and_then(|v| v.get("label"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let artist = entry
                            .get("im:artist")
                            .and_then(|v| v.get("label"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let cover = entry
                            .get("im:image")
                            .and_then(|i| i.as_array())
                            .map(|imgs| {
                                imgs.iter()
                                    .filter_map(|i| i.get("label").and_then(|v| v.as_str()))
                                    .last()
                                    .unwrap_or("")
                                    .replace("/55x55bb.", "/600x600bb.")
                            })
                            .unwrap_or_default();
                        let id = entry
                            .get("id")
                            .and_then(|i| i.get("attributes"))
                            .and_then(|a| a.get("im:id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if !title.is_empty() && !artist.is_empty() {
                            tracks.push(json_track(
                                format!("it-{id}"),
                                title.to_string(),
                                artist.to_string(),
                                cover,
                                180,
                                "ytmusic",
                                "itunes",
                                None,
                            ));
                        }
                    }
                }
            }
        }
    }

    if tracks.len() < 6 && country != "global" {
        let lfm_url = format!(
            "https://ws.audioscrobbler.com/2.0/?method=geo.gettoptracks&country={}&limit=40&api_key={}&format=json",
            util::percent_encode(&country),
            LASTFM_KEY
        );
        if let Ok(resp) = state.http().get(&lfm_url).send().await {
            if let Ok(json) = resp.json::<Value>().await {
                if let Some(items) = json
                    .get("toptracks")
                    .and_then(|t| t.get("track"))
                    .and_then(|t| t.as_array())
                {
                    for item in items.iter().take(30) {
                        let title = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let artist = item
                            .get("artist")
                            .and_then(|a| a.get("name").and_then(|v| v.as_str()))
                            .unwrap_or("");
                        if !title.is_empty() && !artist.is_empty() {
                            tracks.push(json_track(
                                String::new(),
                                title.to_string(),
                                artist.to_string(),
                                String::new(),
                                180,
                                "ytmusic",
                                "lastfm",
                                None,
                            ));
                        }
                    }
                }
            }
        }
    }

    let mut seen = HashMap::new();
    let mut out: Vec<Value> = Vec::new();
    for mut track in tracks.into_iter().take(50) {
        let key = normalized_key(
            track.get("title").and_then(|v| v.as_str()).unwrap_or(""),
            track.get("artist").and_then(|v| v.as_str()).unwrap_or(""),
        );
        if seen.contains_key(&key) {
            continue;
        }
        seen.insert(key, true);
        if let Some(map) = track.as_object_mut() {
            with_listener_fields(map, out.len());
        }
        out.push(track);
    }
    cache_put(&state.stream_cache, cache_key, json!(out), 32, Duration::from_secs(30 * 60));
    json!({ "success": true, "country": country, "tracks": out })
}

pub async fn hot(state: &Shared, _query: &HashMap<String, String>) -> Value {
    if let Some(cached) = cache_get(&state.stream_cache, "disc:hot:global", Duration::from_secs(60 * 60)) {
        return json!({ "success": true, "title": "Hot picks 2026", "tracks": cached });
    }
    let url = "https://api.deezer.com/chart/0/tracks?limit=40";
    let track_list = match state.http().get(url).send().await {
        Ok(resp) => resp
            .json::<Value>()
            .await
            .ok()
            .and_then(|j| j.get("data").and_then(|d| d.as_array()).cloned())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    let mut picks: Vec<Value> = Vec::new();
    for item in track_list.into_iter().take(12) {
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let artist = item
            .get("artist")
            .and_then(|a| a.get("name").and_then(|v| v.as_str()))
            .unwrap_or("");
        if title.is_empty() || artist.is_empty() {
            continue;
        }
        let explicit = item
            .get("explicit_lyrics")
            .or_else(|| item.get("explicit_content_lyrics"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        picks.push(json_track(
            String::new(),
            title.to_string(),
            artist.to_string(),
            item.get("album")
                .and_then(|a| a.get("cover_big").or_else(|| a.get("cover_medium")))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            item.get("duration").and_then(|v| v.as_u64()).unwrap_or(0),
            "youtube",
            "deezer",
            Some(explicit),
        ));
    }
    // Resolve to YouTube ids so playback works — all resolves in flight (12 at once,
    // one pooled GET each; YTM answers them concurrently).
    let resolved_tracks = futures_util::future::join_all(picks.into_iter().map(|track| {
        let st = state.clone();
        async move {
            let artist = track.get("artist").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let title = track.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let explicit_flag = track.get("isExplicit").and_then(|v| v.as_bool());
            let query = format!("{artist} {title}");
            match ytm::search(&st.http(), &query, 1).await {
                Ok(mut tracks) => {
                    if let Some(mut resolved) = tracks.pop() {
                        if let Some(map) = resolved.as_object_mut() {
                            map.insert("source".to_string(), json!("related|deezer"));
                            if let Some(explicit) = explicit_flag {
                                map.insert("isExplicit".to_string(), json!(explicit));
                            }
                        }
                        return resolved;
                    }
                    track
                }
                Err(_) => track,
            }
        }
    })).await;
    let out: Vec<Value> = resolved_tracks.into_iter().collect();
    cache_put(&state.stream_cache, "disc:hot:global".to_string(), json!(out), 4, Duration::from_secs(60 * 60));
    json!({ "success": true, "title": "Hot picks 2026", "tracks": out })
}

// --------------------------------------------------------------- artist

pub async fn artist(state: &Shared, query: &HashMap<String, String>) -> Value {
    let name = match query.get("name") {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => return json!({ "success": false, "error": "Missing artist name" }),
    };
    let cache_key = format!("disc:artist:{name}");
    if let Some(cached) = cache_get(&state.stream_cache, &cache_key, Duration::from_secs(24 * 60 * 60)) {
        return json!({ "success": true, "artist": cached });
    }

    let encoded = util::percent_encode(&name);
    let search_url = format!("https://api.deezer.com/search/artist?q={encoded}&limit=10");
    let artist_id: Option<u64> = match state.http().get(&search_url).send().await {
        Ok(resp) => resp
            .json::<Value>()
            .await
            .ok()
            .and_then(|j| {
                j.get("data")
                    .and_then(|d| d.as_array())
                    .and_then(|items| {
                        items.iter().find(|item| {
                            item.get("name").and_then(|v| v.as_str()).map(|n| n.eq_ignore_ascii_case(&name)).unwrap_or(false)
                        })
                        .or_else(|| items.first())
                        .and_then(|a| a.get("id").and_then(|v| v.as_u64()))
                    })
            }),
        Err(_) => None,
    };

    let Some(artist_id) = artist_id else {
        return json!({ "success": false, "error": "artist not found" });
    };

    let (top, albums_raw, info_raw) = futures_util::future::join3(
        state.http().get(format!("https://api.deezer.com/artist/{artist_id}/top?limit=100")).send(),
        state.http().get(format!("https://api.deezer.com/artist/{artist_id}/albums?limit=40")).send(),
        state.http().get(format!("https://api.deezer.com/artist/{artist_id}")).send(),
    )
    .await;

    let mut tracks: Vec<Value> = Vec::new();
    if let Ok(resp) = top {
        if let Ok(json) = resp.json::<Value>().await {
            if let Some(items) = json.get("data").and_then(|d| d.as_array()) {
                for item in items {
                    let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    if title.is_empty() {
                        continue;
                    }
                    let cover = item
                        .get("cover_big")
                        .or_else(|| item.get("cover_medium"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let rank = item.get("rank").and_then(|v| v.as_u64()).unwrap_or(0);
                    let explicit = item.get("explicit_lyrics").and_then(|v| v.as_bool()).unwrap_or(false);
                    tracks.push(json_track(
                        format!("dz-{}", item.get("id").and_then(|v| v.as_u64()).unwrap_or(0)),
                        title.to_string(),
                        name.clone(),
                        cover.to_string(),
                        item.get("duration").and_then(|v| v.as_u64()).unwrap_or(0),
                        "youtube",
                        "deezer",
                        Some(explicit),
                    ));
                    let _ = rank;
                }
            }
        }
    }
    tracks.truncate(100);

    let mut albums: Vec<Value> = Vec::new();
    if let Ok(resp) = albums_raw {
        if let Ok(json) = resp.json::<Value>().await {
            if let Some(items) = json.get("data").and_then(|d| d.as_array()) {
                for item in items {
                    let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
                    if title.is_empty() {
                        continue;
                    }
                    let year = item
                        .get("release_date")
                        .and_then(|v| v.as_str())
                        .and_then(|d| d.get(..4).map(String::from))
                        .unwrap_or_default();
                    let rank_cover = item
                        .get("cover_big")
                        .or_else(|| item.get("cover_medium"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let record_type = item.get("record_type").and_then(|v| v.as_str()).unwrap_or("album").to_string();
                    albums.push(json!({
                        "id": format!("dz-album-{}", item.get("id").and_then(|v| v.as_u64()).unwrap_or(0)),
                        "title": title,
                        "artist": name,
                        "cover": rank_cover,
                        "year": year,
                        "trackCount": item.get("nb_tracks").and_then(|v| v.as_u64()).unwrap_or(0),
                        "recordType": record_type,
                        "source": "deezer",
                    }));
                }
            }
        }
    }

    let mut fans = 0u64;
    let mut avatar = String::new();
    let mut bio = String::new();
    if let Ok(resp) = info_raw {
        if let Ok(json) = resp.json::<Value>().await {
            fans = json.get("nb_fan").and_then(|v| v.as_u64()).unwrap_or(0);
            avatar = json.get("picture_big").and_then(|p| p.as_str()).unwrap_or("").to_string();
        }
    }

    // Bio + monthly listeners from Last.fm (public API).
    let mut monthly_listeners = String::new();
    let lfm_url = format!(
        "https://ws.audioscrobbler.com/2.0/?method=artist.getinfo&artist={}&api_key=4cb074e4b8ec4ee9ad3eb37d6f7eb240&format=json",
        util::percent_encode(&name)
    );
    if let Ok(resp) = state.http().get(&lfm_url).send().await {
        if let Ok(json) = resp.json::<Value>().await {
            if let Some(art) = json.get("artist") {
                bio = art
                    .get("bio")
                    .and_then(|b| b.get("summary").and_then(|s| s.as_str()))
                    .map(|s| s.split("<a href").next().unwrap_or(s).trim().to_string())
                    .unwrap_or_default();
                monthly_listeners = art
                    .get("stats")
                    .and_then(|s| s.get("listeners").and_then(|l| l.as_str()))
                    .map(|l| l.to_string())
                    .unwrap_or_default();
            }
        }
    }

    let banners = vec![avatar.clone()];
    let is_verified = fans > 30_000 || (fans > 12_000 && tracks.len() >= 5);
    let monthly_listeners_text = if monthly_listeners.is_empty() {
        String::new()
    } else {
        format!("{monthly_listeners} monthly listeners")
    };

    let artist_obj = json!({
        "name": name,
        "bio": bio,
        "avatar": avatar,
        "banner": banners.first().cloned().unwrap_or_default(),
        "banners": banners,
        "albums": albums,
        "isVerified": is_verified,
        "isKnownArtist": is_verified || tracks.len() >= 5,
        "fans": fans,
        "monthlyListeners": monthly_listeners_text,
        "tracks": tracks,
    });
    cache_put(&state.stream_cache, cache_key.clone(), artist_obj.clone(), 35, Duration::from_secs(24 * 60 * 60));
    json!({ "success": true, "artist": artist_obj })
}
