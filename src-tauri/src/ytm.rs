//! YouTube Music inner API search.
//!
//! Upstream spawns `yt-dlp` for every search. That is a whole Python process
//! (tens of MB, ~1s of CPU) for data YouTube exposes over a normal JSON POST.
//! This module is the fast path; `/api/search` still falls back to yt-dlp when
//! the inner API changes or returns nothing.

use serde_json::{json, Value};

const SEARCH_URL: &str = "https://music.youtube.com/youtubei/v1/search?prettyPrint=false";
const CLIENT_VERSION: &str = "1.20250101.01.00";
/// "Songs" filter for the WEB_REMIX search endpoint.
const SONGS_PARAMS: &str = "EgWKAQIIAWoMEA4QChADEAQQCRAF";

pub async fn search(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let body = json!({
        "context": {
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": CLIENT_VERSION,
                "hl": "en",
                "gl": "US",
            }
        },
        "query": query,
        "params": SONGS_PARAMS,
    });
    let response = client
        .post(SEARCH_URL)
        .header("Content-Type", "application/json")
        .header("Origin", "https://music.youtube.com")
        .header("Referer", "https://music.youtube.com/")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ytmusic request failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("ytmusic status {}", response.status().as_u16()));
    }
    let payload: Value = response
        .json()
        .await
        .map_err(|e| format!("ytmusic parse error: {e}"))?;
    Ok(parse_songs(&payload, limit))
}

fn parse_songs(payload: &Value, limit: usize) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for item in section_items(payload) {
        let Some(renderer) = item.get("musicResponsiveListItemRenderer") else {
            continue;
        };
        let Some(video_id) = video_id(renderer) else {
            continue;
        };
        if !valid_video_id(&video_id) || !seen.insert(video_id.clone()) {
            continue;
        }

        let title = join_runs(&flex_runs(renderer, 0));
        if title.is_empty() {
            continue;
        }

        let metadata = join_runs(&flex_runs(renderer, 1));
        let mut segments: Vec<String> = metadata
            .split('•')
            .map(|part| part.trim().to_string())
            .filter(|part| {
                !part.is_empty()
                    && !part.eq_ignore_ascii_case("song")
                    && !part.eq_ignore_ascii_case("video")
            })
            .collect();

        let mut duration = 0u64;
        if let Some(last) = segments.last() {
            let parsed = parse_duration(last);
            if parsed > 0 {
                duration = parsed;
                segments.pop();
            }
        }
        if duration > 0 && duration < 30 {
            continue;
        }

        let artist = segments
            .first()
            .map(|s| strip_topic_suffix(s))
            .unwrap_or_default();
        let album = segments.get(1).cloned().unwrap_or_default();
        let thumbnail = thumbnail(renderer, &video_id);

        out.push(json!({
            "id": video_id,
            "title": title,
            "artist": if artist.is_empty() { "Unknown Artist".to_string() } else { artist },
            "album": album,
            "duration": duration,
            "thumbnail": thumbnail,
            "url": format!("https://www.youtube.com/watch?v={video_id}"),
            "platform": "ytmusic",
            "source": "youtube",
            "isExplicit": is_explicit(renderer),
        }));
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn section_items(payload: &Value) -> Vec<&Value> {
    let mut sections: Vec<&Value> = Vec::new();
    if let Some(tabs) = payload
        .pointer("/contents/tabbedSearchResultsRenderer/tabs")
        .and_then(Value::as_array)
    {
        for tab in tabs {
            if let Some(contents) = tab
                .pointer("/tabRenderer/content/sectionListRenderer/contents")
                .and_then(Value::as_array)
            {
                sections.extend(contents.iter());
            }
        }
    }
    if sections.is_empty() {
        if let Some(contents) = payload
            .pointer("/contents/sectionListRenderer/contents")
            .and_then(Value::as_array)
        {
            sections.extend(contents.iter());
        }
    }

    let mut items = Vec::new();
    for section in sections {
        for key in [
            "musicShelfRenderer",
            "itemSectionRenderer",
            "musicCardShelfRenderer",
        ] {
            if let Some(contents) = section
                .get(key)
                .and_then(|value| value.get("contents"))
                .and_then(Value::as_array)
            {
                items.extend(contents.iter());
            }
        }
    }
    items
}

fn flex_runs<'a>(renderer: &'a Value, index: usize) -> Vec<&'a Value> {
    renderer
        .pointer(&format!(
            "/flexColumns/{index}/musicResponsiveListItemFlexColumnRenderer/text/runs"
        ))
        .and_then(Value::as_array)
        .map(|runs| runs.iter().collect())
        .unwrap_or_default()
}

fn join_runs(runs: &[&Value]) -> String {
    runs.iter()
        .filter_map(|run| run.get("text").and_then(Value::as_str))
        .collect::<String>()
        .trim()
        .to_string()
}

fn video_id(renderer: &Value) -> Option<String> {
    renderer
        .pointer("/playlistItemData/videoId")
        .or_else(|| {
            renderer.pointer(
                "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId",
            )
        })
        .and_then(Value::as_str)
        .map(|id| id.to_string())
}

fn valid_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn parse_duration(text: &str) -> u64 {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return 0;
    }
    if !parts
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        return 0;
    }
    parts
        .iter()
        .fold(0u64, |acc, part| acc * 60 + part.parse::<u64>().unwrap_or(0))
}

fn strip_topic_suffix(artist: &str) -> String {
    let trimmed = artist.trim();
    if trimmed.to_ascii_lowercase().ends_with(" - topic") {
        trimmed[..trimmed.len() - " - topic".len()].trim().to_string()
    } else {
        trimmed.to_string()
    }
}

fn thumbnail(renderer: &Value, video_id: &str) -> String {
    let raw = renderer
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .and_then(|thumbs| thumbs.last())
        .and_then(|thumb| thumb.get("url"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if raw.is_empty() {
        return format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg");
    }
    if let Some(index) = raw.find("=w") {
        format!("{}=w544-h544", &raw[..index])
    } else {
        raw.to_string()
    }
}

fn is_explicit(renderer: &Value) -> bool {
    renderer
        .get("badges")
        .and_then(Value::as_array)
        .map(|badges| {
            badges.iter().any(|badge| {
                badge
                    .pointer("/musicInlineBadgeRenderer/icon/iconType")
                    .and_then(Value::as_str)
                    == Some("MUSIC_EXPLICIT_BADGE")
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_song_shelf() {
        let payload = json!({
            "contents": {
                "tabbedSearchResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {
                            "content": {
                                "sectionListRenderer": {
                                    "contents": [{
                                        "musicShelfRenderer": {
                                            "contents": [{
                                                "musicResponsiveListItemRenderer": {
                                                    "playlistItemData": { "videoId": "dQw4w9WgXcQ" },
                                                    "flexColumns": [
                                                        { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": "Never Gonna Give You Up" }] } } },
                                                        { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [
                                                            { "text": "Rick Astley" },
                                                            { "text": " • " },
                                                            { "text": "Whenever You Need Somebody" },
                                                            { "text": " • " },
                                                            { "text": "3:33" }
                                                        ] } } }
                                                    ],
                                                    "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                                                        { "url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg=w60-h60" }
                                                    ] } } }
                                                }
                                            }]
                                        }
                                    }]
                                }
                            }
                        }
                    }]
                }
            }
        });
        let tracks = parse_songs(&payload, 10);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0]["id"], "dQw4w9WgXcQ");
        assert_eq!(tracks[0]["title"], "Never Gonna Give You Up");
        assert_eq!(tracks[0]["artist"], "Rick Astley");
        assert_eq!(tracks[0]["duration"], 213);
        assert_eq!(tracks[0]["platform"], "ytmusic");
    }

    #[test]
    fn duration_parser_is_strict() {
        assert_eq!(parse_duration("3:33"), 213);
        assert_eq!(parse_duration("1:02:03"), 3723);
        assert_eq!(parse_duration("live"), 0);
        assert_eq!(parse_duration("3:zz"), 0);
    }
}
