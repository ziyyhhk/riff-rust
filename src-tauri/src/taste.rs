//! Listening taste / genre profile engine.
//!
//! The renderer used to scan its whole history in JavaScript and guess seeds
//! from artist names. This module owns that work in Rust: it normalises
//! tracks, decays plays by recency, resolves Last.fm genre/mood tags (cached),
//! and returns the seed list the `/api/vibe` queue filter ranks on. It also
//! owns the shared title/artist normalisation used for remix-family blocking.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};

use crate::server::Shared;
use crate::state::{cache_get, cache_put};
use crate::util;

const LASTFM_KEY: &str = "4b4f73bda181868353f9b438604adf52";
const TAG_TTL: Duration = Duration::from_secs(24 * 60 * 60);

fn clean_words(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn normalize_artist(name: &str) -> String {
    let lower = name.trim().replace('ё', "е").to_lowercase();
    let lower = lower
        .trim_end_matches("- topic")
        .trim_end_matches(" - topic")
        .trim()
        .to_string();
    let first = lower
        .split(|c: char| c == ',' || c == ';' || c == '&' || c == '/')
        .next()
        .unwrap_or(&lower)
        .trim();
    // Cut a collaboration suffix: "artist feat. other", "artist x other".
    let first = first
        .split(|c: char| c == ' ' || c == '\u{00d7}')
        .collect::<Vec<_>>();
    let mut kept: Vec<&str> = Vec::new();
    for word in first {
        if matches!(
            word,
            "feat" | "feat." | "ft" | "ft." | "featuring" | "with" | "x" | "vs" | "vs."
        ) {
            break;
        }
        kept.push(word);
    }
    let joined = kept.join(" ");
    let words: Vec<&str> = joined
        .split_whitespace()
        .filter(|w| !matches!(*w, "vevo" | "official" | "records" | "topic" | "remix"))
        .collect();
    clean_words(&words.join(" "))
}

/// Core song name with version decorators removed. "DrollXD (Slowed + Reverb)"
/// and "DrollXD" collapse to the same string, so the queue can keep at most
/// one cut of the song.
pub fn core_title(title: &str) -> String {
    let lower = title.trim().replace('ё', "е").to_lowercase();
    let mut stripped = String::with_capacity(lower.len());
    let mut depth = 0i32;
    for ch in lower.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {
                if depth <= 0 {
                    stripped.push(ch);
                }
            }
        }
    }
    let tokens: Vec<&str> = stripped.split_whitespace().collect();
    let mut kept: Vec<&str> = Vec::new();
    for token in tokens {
        if matches!(token, "feat" | "feat." | "ft" | "ft." | "featuring") {
            break;
        }
        if matches!(
            token,
            "slowed"
                | "reverb"
                | "sped"
                | "speed"
                | "up"
                | "down"
                | "nightcore"
                | "remix"
                | "remixed"
                | "edit"
                | "version"
                | "prod"
                | "produced"
                | "official"
                | "audio"
                | "video"
                | "lyrics"
                | "tekk"
                | "ultra"
                | "bass"
                | "boosted"
                | "extended"
                | "radio"
                | "with"
        ) {
            continue;
        }
        kept.push(token);
    }
    clean_words(&kept.join(" "))
}

fn contains_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let lower = hay.to_lowercase();
    let needle = needle.to_lowercase();
    let mut start = 0usize;
    while start < lower.len() {
        let Some(rel) = lower[start..].find(&needle) else {
            return false;
        };
        let idx = start + rel;
        let prev = lower[..idx].chars().next_back();
        let next = lower[idx + needle.len()..].chars().next();
        let before_ok = !prev.map(|c| c.is_alphanumeric()).unwrap_or(false);
        let after_ok = !next.map(|c| c.is_alphanumeric()).unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        start = idx + needle.len().max(1);
    }
    false
}

/// Map a raw tag onto the canonical vocabulary. Returns `None` for Last.fm
/// junk ("seen live", artist names) and tags outside the vocabulary.
pub fn canonical_tag(raw: &str) -> Option<String> {
    let normalized = clean_words(raw);
    let mapped = match normalized.as_str() {
        "breakcore" => "breakcore",
        "drum and bass" | "drum & bass" | "drum n bass" | "drumnbass" | "dnb" | "drumstep" => "dnb",
        "jungle" => "jungle",
        "hyperpop" => "hyperpop",
        "phonk" | "drift phonk" => "phonk",
        "psytrance" | "psy trance" | "psychedelic trance" => "psytrance",
        "trance" => "trance",
        "techno" => "techno",
        "house" => "house",
        "dubstep" => "dubstep",
        "riddim" => "riddim",
        "hardstyle" => "hardstyle",
        "hardcore" | "gabber" | "uk hardcore" => "hardcore",
        "metal" | "heavy metal" | "metalcore" | "deathcore" => "metal",
        "rock" | "classic rock" | "alternative rock" | "indie rock" | "hard rock" => "rock",
        "indie" | "indietronica" => "indie",
        "pop" | "synthpop" | "dream pop" | "art pop" => "pop",
        "hiphop" | "hip hop" | "hip-hop" | "rap" | "gangsta rap" | "trap" | "grime" => "hiphop",
        "rnb" | "r&b" | "rhythm and blues" => "rnb",
        "soul" | "neo soul" => "soul",
        "funk" => "funk",
        "jazz" | "nu jazz" => "jazz",
        "classical" | "orchestral" | "piano" => "classical",
        "ambient" | "ambient techno" | "drone" => "ambient",
        "lofi" | "lo-fi" | "chillhop" => "lofi",
        "edm" | "electronic" | "electronica" | "electro" | "dance" | "idm" => "electronic",
        "experimental" | "avant garde" | "glitch" => "experimental",
        "noise" | "power electronics" => "noise",
        "industrial" | "ebm" => "industrial",
        "synthwave" | "synth pop" => "synthwave",
        "vaporwave" => "vaporwave",
        "shoegaze" => "shoegaze",
        "punk" | "pop punk" | "post punk" => "punk",
        "ska" | "reggae" | "dancehall" => "reggae",
        "afrobeat" | "afrobeats" => "afrobeat",
        "latin" | "reggaeton" | "latin pop" => "latin",
        "k-pop" | "kpop" => "kpop",
        "j-pop" | "jpop" => "jpop",
        "folk" | "country" => "folk",
        "blues" => "blues",
        "gospel" => "gospel",
        "sad" | "sadness" | "melancholy" | "melancholic" | "depressing" | "depressed" => "sad",
        "emotional" | "sentimental" => "emotional",
        "heartbreak" | "heartbroken" => "heartbreak",
        "lonely" | "alone" | "isolation" => "lonely",
        "happy" | "happiness" | "upbeat" | "cheerful" | "feel good" => "happy",
        "chill" | "chillout" | "relaxing" | "mellow" | "calm" | "laid back" => "chill",
        "dark" | "darkwave" | "gothic" | "goth" => "dark",
        "aggressive" | "angry" | "rage" => "aggressive",
        "hype" | "energetic" | "high energy" | "party" => "hype",
        "dreamy" | "ethereal" | "dream" => "dreamy",
        "nostalgic" | "nostalgia" | "retro" => "nostalgic",
        "romantic" | "love" | "romance" => "romantic",
        "peaceful" | "serene" | "meditative" => "peaceful",
        "atmospheric" | "cinematic" | "soundtrack" => "atmospheric",
        _ => return None,
    };
    Some(mapped.to_string())
}

/// Fast offline genre/mood classification from a track's title and artist.
pub fn classify_text(title: &str, artist: &str) -> Vec<String> {
    let hay = format!("{title} {artist}");
    let checks: &[(&str, &str)] = &[
        ("drum and bass", "dnb"),
        ("drum & bass", "dnb"),
        ("drum n bass", "dnb"),
        ("drumnbass", "dnb"),
        ("breakcore", "breakcore"),
        ("hyperpop", "hyperpop"),
        ("phonk", "phonk"),
        ("psy trance", "psytrance"),
        ("psychedelic trance", "psytrance"),
        ("psytrance", "psytrance"),
        ("jungle", "jungle"),
        ("riddim", "riddim"),
        ("hardstyle", "hardstyle"),
        ("hardcore", "hardcore"),
        ("gabber", "hardcore"),
        ("dubstep", "dubstep"),
        ("techno", "techno"),
        ("trance", "trance"),
        ("house", "house"),
        ("edm", "edm"),
        ("electronic", "electronic"),
        ("ambient", "ambient"),
        ("classical", "classical"),
        ("jazz", "jazz"),
        ("soul", "soul"),
        ("funk", "funk"),
        ("hip hop", "hiphop"),
        ("hip-hop", "hiphop"),
        ("rap", "hiphop"),
        ("rock", "rock"),
        ("metal", "metal"),
        ("indie", "indie"),
        ("pop", "pop"),
        ("noise", "noise"),
        ("industrial", "industrial"),
        ("synthwave", "synthwave"),
        ("vaporwave", "vaporwave"),
        ("shoegaze", "shoegaze"),
        ("lo-fi", "lofi"),
        ("lofi", "lofi"),
        ("slowed and reverb", "chill"),
        ("slowed + reverb", "chill"),
        ("sped up", "hype"),
        ("nightcore", "hype"),
        ("sad", "sad"),
        ("melancholy", "sad"),
        ("melancholic", "sad"),
        ("heartbreak", "heartbreak"),
        ("heartbroken", "heartbreak"),
        ("lonely", "lonely"),
        ("alone", "lonely"),
        ("emotional", "emotional"),
        ("happy", "happy"),
        ("upbeat", "happy"),
        ("chill", "chill"),
        ("relaxing", "chill"),
        ("mellow", "chill"),
        ("calm", "chill"),
        ("dark", "dark"),
        ("aggressive", "aggressive"),
        ("angry", "aggressive"),
        ("hype", "hype"),
        ("energetic", "hype"),
        ("dreamy", "dreamy"),
        ("ethereal", "dreamy"),
        ("nostalgic", "nostalgic"),
        ("nostalgia", "nostalgic"),
        ("romantic", "romantic"),
        ("love", "romantic"),
        ("peaceful", "peaceful"),
        ("atmospheric", "atmospheric"),
    ];
    let mut out: Vec<String> = Vec::new();
    for (needle, tag) in checks {
        if contains_word(&hay, needle) && !out.iter().any(|t| t == tag) {
            out.push((*tag).to_string());
        }
    }
    out
}

fn is_genre(tag: &str) -> bool {
    !matches!(
        tag,
        "sad" | "emotional" | "heartbreak" | "lonely" | "happy" | "chill" | "dark" | "aggressive"
            | "hype" | "dreamy" | "nostalgic" | "romantic" | "peaceful" | "atmospheric"
    )
}

async fn lastfm_tags(state: &Shared, method: &str, artist: &str, track: Option<&str>) -> Vec<String> {
    let artist_part = util::percent_encode(artist);
    let url = match track {
        Some(title) if !title.trim().is_empty() => format!(
            "https://ws.audioscrobbler.com/2.0/?method={method}&artist={artist_part}&track={}&api_key={}&format=json",
            util::percent_encode(title),
            LASTFM_KEY
        ),
        _ => format!(
            "https://ws.audioscrobbler.com/2.0/?method={method}&artist={artist_part}&api_key={}&format=json",
            LASTFM_KEY
        ),
    };
    let Ok(resp) = state.http().get(&url).send().await else {
        return Vec::new();
    };
    let Ok(json) = resp.json::<Value>().await else {
        return Vec::new();
    };
    let Some(tags) = json.get("toptags").and_then(|t| t.get("tag")).and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for tag in tags.iter().take(16) {
        let name = tag.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let count = tag.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
        if count < 20 {
            continue;
        }
        if let Some(canonical) = canonical_tag(name) {
            if !out.iter().any(|t| t == &canonical) {
                out.push(canonical);
            }
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

fn cache_value_to_tags(value: Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn tags_to_cache(tags: &[String]) -> Value {
    Value::Array(tags.iter().map(|t| json!(t)).collect())
}

pub async fn track_tags(state: &Shared, artist: &str, title: &str) -> Vec<String> {
    let norm = format!("tags:track:{}:{}", normalize_artist(artist), core_title(title));
    if let Some(tags) = cache_get(&state.stream_cache, &norm, TAG_TTL) {
        return cache_value_to_tags(tags);
    }
    let tags = lastfm_tags(state, "track.gettoptags", artist, Some(title)).await;
    cache_put(&state.stream_cache, norm, tags_to_cache(&tags), 1024, TAG_TTL);
    tags
}

pub async fn artist_tags(state: &Shared, artist: &str) -> Vec<String> {
    let norm = format!("tags:artist:{}", normalize_artist(artist));
    if let Some(tags) = cache_get(&state.stream_cache, &norm, TAG_TTL) {
        return cache_value_to_tags(tags);
    }
    let tags = lastfm_tags(state, "artist.gettoptags", artist, None).await;
    cache_put(&state.stream_cache, norm, tags_to_cache(&tags), 1024, TAG_TTL);
    tags
}

#[derive(Clone)]
struct TasteEntry {
    artist: String,
    title: String,
    id: String,
    platform: String,
    weight: f64,
    timestamp: i64,
}

fn push_tags(
    tags: &[String],
    weight: f64,
    genres: &mut HashMap<String, f64>,
    moods: &mut HashMap<String, f64>,
) {
    for tag in tags {
        if is_genre(tag) {
            *genres.entry(tag.clone()).or_insert(0.0) += weight;
        } else {
            *moods.entry(tag.clone()).or_insert(0.0) += weight;
        }
    }
}

fn sort_tags(map: &HashMap<String, f64>, limit: usize) -> Vec<Value> {
    let mut items: Vec<(String, f64)> = map.iter().map(|(k, v)| (k.clone(), *v)).collect();
    items.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    items
        .into_iter()
        .take(limit)
        .map(|(name, weight)| json!({ "name": name, "weight": (weight * 100.0).round() / 100.0 }))
        .collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub async fn profile(state: &Shared, body: &Value) -> Value {
    let history = body.get("history").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let favorites = body.get("favorites").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let max_seeds = body
        .get("maxSeeds")
        .and_then(|v| v.as_u64())
        .unwrap_or(8)
        .clamp(1, 12) as usize;
    let window_days = body.get("windowDays").and_then(|v| v.as_u64()).unwrap_or(7).max(1) as f64;

    let skip_artists: Vec<String> = body
        .get("skipArtists")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(normalize_artist))
        .filter(|s| !s.is_empty())
        .collect();
    let skip_keys: Vec<String> = body
        .get("skipKeys")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(|k| {
            let (artist, title) = k.split_once("::").unwrap_or(("", k));
            format!("{}::{}", normalize_artist(artist), core_title(title))
        })
        .collect();

    let mut entries: HashMap<String, TasteEntry> = HashMap::new();
    let now = now_ms();

    let mut add_entry = |track: &Value, base_weight: f64, index: usize, is_favorite: bool| {
        let artist = track.get("artist").and_then(|v| v.as_str()).unwrap_or("").trim();
        let title = track.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
        if artist.is_empty() || title.is_empty() {
            return;
        }
        let norm_artist = normalize_artist(artist);
        if skip_artists.iter().any(|a| *a == norm_artist) {
            return;
        }
        let raw_id = track.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
        let id = if raw_id.is_empty() {
            String::new()
        } else {
            raw_id.to_string()
        };
        let key = if !id.is_empty() {
            format!("id:{id}")
        } else {
            format!("{}::{}", norm_artist, core_title(title))
        };
        if skip_keys.iter().any(|k| *k == format!("{}::{}", norm_artist, core_title(title))) {
            return;
        }
        let timestamp = track
            .get("playedAt")
            .or_else(|| track.get("addedAt"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let recency = if timestamp > 0 && timestamp <= now + 60_000 {
            let age_days = ((now - timestamp) as f64 / 86_400_000.0).max(0.0);
            0.5_f64.powf(age_days / 10.0)
        } else {
            0.94_f64.powi(index as i32)
        };
        // Recent listening dominates the profile, so "For you" tracks what
        // the user plays now instead of only their all-time favourites.
        let recent_boost = if timestamp > 0 {
            let age_days = ((now - timestamp) as f64 / 86_400_000.0).max(0.0);
            0.25 + 0.75 * 0.5_f64.powf(age_days / window_days)
        } else {
            0.8
        };
        let mut weight = base_weight * recency * recent_boost;
        if is_favorite {
            weight *= 1.5;
        }
        weight = weight.max(0.0001);
        let entry = entries.entry(key).or_insert_with(|| TasteEntry {
            artist: artist.to_string(),
            title: title.to_string(),
            id: id.clone(),
            platform: track
                .get("platform")
                .and_then(|v| v.as_str())
                .unwrap_or("youtube")
                .to_string(),
            weight: 0.0,
            timestamp,
        });
        if weight > entry.weight {
            entry.weight = weight;
            entry.artist = artist.to_string();
            entry.title = title.to_string();
        }
        if id.is_empty() {
            entry.id = raw_id.to_string();
        }
        if timestamp > entry.timestamp {
            entry.timestamp = timestamp;
        }
        entry.platform = track
            .get("platform")
            .and_then(|v| v.as_str())
            .unwrap_or("youtube")
            .to_string();
    };

    for (index, track) in favorites.iter().enumerate() {
        add_entry(track, 4.0, index, true);
    }
    for (index, track) in history.iter().enumerate() {
        add_entry(track, 1.0, index, false);
    }

    let mut ordered: Vec<TasteEntry> = entries.into_values().collect();
    ordered.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut top_artists: Vec<String> = Vec::new();
    let mut artist_weights: HashMap<String, f64> = HashMap::new();
    let mut seen_artists: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in &ordered {
        let norm = normalize_artist(&entry.artist);
        if norm.is_empty() {
            continue;
        }
        *artist_weights.entry(norm.clone()).or_insert(0.0) += entry.weight;
        if seen_artists.insert(norm) {
            top_artists.push(entry.artist.clone());
            if top_artists.len() >= 8 {
                break;
            }
        }
    }
    top_artists.truncate(5);

    let mut seeds: Vec<Value> = Vec::new();
    let mut seed_artists: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in &ordered {
        let norm = normalize_artist(&entry.artist);
        if norm.is_empty() || !seed_artists.insert(norm) {
            continue;
        }
        seeds.push(json!({
            "artist": entry.artist,
            "title": entry.title,
            "id": entry.id,
            "platform": entry.platform,
            "weight": (entry.weight * 1000.0).round() / 1000.0,
        }));
        if seeds.len() >= max_seeds {
            break;
        }
    }

    let mut genres: HashMap<String, f64> = HashMap::new();
    let mut moods: HashMap<String, f64> = HashMap::new();
    let mut daily_genres: HashMap<String, f64> = HashMap::new();
    let mut daily_moods: HashMap<String, f64> = HashMap::new();

    for entry in &ordered {
        let tags = classify_text(&entry.title, &entry.artist);
        let weight = entry.weight.min(8.0);
        push_tags(&tags, weight, &mut genres, &mut moods);
        let is_recent = entry.timestamp > 0
            && (now - entry.timestamp) < (window_days as i64) * 86_400_000;
        if is_recent {
            push_tags(&tags, weight, &mut daily_genres, &mut daily_moods);
        }
    }

    // Enrich the top seeds/artists with Last.fm tags. These calls are cached
    // and bounded, so a cold profile only costs one short parallel batch.
    let seed_list: Vec<(String, String)> = seeds
        .iter()
        .take(5)
        .filter_map(|s| {
            let artist = s.get("artist").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let title = s.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if artist.is_empty() {
                None
            } else {
                Some((artist, title))
            }
        })
        .collect();
    let tag_futures: Vec<_> = seed_list
        .iter()
        .map(|(artist, title)| {
            let state = state.clone();
            let artist = artist.clone();
            let title = title.clone();
            async move {
                let mut tags = track_tags(&state, &artist, &title).await;
                if tags.is_empty() {
                    tags = artist_tags(&state, &artist).await;
                }
                tags
            }
        })
        .collect();
    let tag_results = match tokio::time::timeout(Duration::from_millis(2500), futures_util::future::join_all(tag_futures)).await {
        Ok(results) => results,
        Err(_) => Vec::new(),
    };
    for tags in tag_results {
        push_tags(&tags, 2.0, &mut genres, &mut moods);
    }

    let mut durations: Vec<u64> = Vec::new();
    for track in history.iter().chain(favorites.iter()) {
        if let Some(duration) = track.get("duration").and_then(|v| v.as_u64()) {
            if duration > 0 {
                durations.push(duration);
            }
        }
    }
    let avg_duration = if durations.is_empty() {
        Value::Null
    } else {
        json!(durations.iter().sum::<u64>() / durations.len() as u64)
    };

    let text: String = history
        .iter()
        .chain(favorites.iter())
        .filter_map(|t| t.get("title").and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    let cyrillic = text.chars().filter(|c| ('\u{0400}'..='\u{04FF}').contains(c)).count();
    let dominant_script = if !text.is_empty() && cyrillic * 100 > text.chars().count() * 35 {
        "Cyrillic"
    } else {
        "Latin"
    };

    let profile = json!({
        "trackCount": history.len() + favorites.len(),
        "topArtists": top_artists,
        "genres": sort_tags(&genres, 6),
        "moods": sort_tags(&moods, 4),
        "dailyGenres": sort_tags(&daily_genres, 6),
        "dailyMoods": sort_tags(&daily_moods, 4),
        "seeds": seeds,
        "dominantScript": dominant_script,
        "avgDuration": avg_duration,
        "windowDays": window_days as u64,
    });

    json!({ "success": true, "profile": profile })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_title_strips_version_decorators() {
        assert_eq!(core_title("DrollXD (Slowed + Reverb)"), "drollxd");
        assert_eq!(core_title("DrollXD TEKK Ultra Slowed"), "drollxd");
        assert_eq!(core_title("Song feat. Someone"), "song");
        assert_eq!(core_title("Anybody Can Find Love (breakcore, instrumental)"), "anybody can find love");
    }

    #[test]
    fn canonical_tags_map_synonyms_and_reject_junk() {
        assert_eq!(canonical_tag("Drum and Bass").as_deref(), Some("dnb"));
        assert_eq!(canonical_tag("melancholy").as_deref(), Some("sad"));
        assert_eq!(canonical_tag("seen live"), None);
        assert_eq!(canonical_tag("Spotify"), None);
    }

    #[test]
    fn classify_finds_genres_and_moods() {
        let tags = classify_text("anybody can find love (breakcore, instrumental)", "breakcore girl");
        assert!(tags.iter().any(|t| t == "breakcore"));
        let tags = classify_text("heartbreak avenue", "someone");
        assert!(tags.iter().any(|t| t == "heartbreak"));
        assert!(!classify_text("popular", "someone").iter().any(|t| t == "pop"));
    }
}
