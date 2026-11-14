use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use crate::util;

pub fn load_library() -> Vec<Value> {
    match util::read_json_file(&util::library_path()) {
        Value::Array(items) => items,
        Value::Object(map) => map.get("tracks").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub fn save_library(tracks: &[Value]) {
    let _ = util::write_json_file(&util::library_path(), &json!({ "tracks": tracks }));
}

pub fn find_track_by_id(id: &str) -> Option<PathBuf> {
    let clean = id.trim();
    // `local_<hash>` ids map to `library/<id>.<ext>`
    if clean.starts_with("local_") || clean.starts_with("riff-") {
        if let Ok(entries) = fs::read_dir(util::library_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(clean) {
                    return Some(entry.path());
                }
            }
        }
        // Also check the download cache (yt-dlp output `<id>.<ext>`)
        if let Ok(entries) = fs::read_dir(util::cache_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(clean) {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}

pub fn find_cached_by_id(id: &str) -> Option<PathBuf> {
    find_track_by_id(id)
}

/// Copy audio files into the library dir and return display-ready items
/// (same shape the Electron importer produced: {id, file, ext, name}).
pub fn import_audio_files(paths: &[String]) -> Vec<Value> {
    const AUDIO_EXT: [&str; 10] = ["mp3", "wav", "flac", "opus", "ogg", "oga", "m4a", "aac", "webm", "weba"];
    let mut sources: Vec<PathBuf> = Vec::new();
    for p in paths {
        let path = PathBuf::from(p);
        if path.is_dir() {
            if let Ok(entries) = fs::read_dir(&path) {
                for entry in entries.flatten() {
                    let ep = entry.path();
                    if ep.is_file() {
                        if let Some(ext) = ep.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) {
                            if AUDIO_EXT.contains(&ext.as_str()) {
                                sources.push(ep);
                            }
                        }
                    }
                }
            }
        } else if path.is_file() {
            sources.push(path);
        }
    }

    let _ = fs::create_dir_all(util::library_dir());
    let mut out = Vec::new();
    for src in sources {
        let meta = match fs::metadata(&src) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ext = src.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_else(|| "mp3".into());
        let stem = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "track".into());
        let hash = util::sha1_hex(format!("{}{}", stem, meta.len()).as_bytes());
        let id = format!("local_{}", &hash[..12]);
        let file = format!("{id}.{ext}");
        let dest = util::library_dir().join(&file);
        if !dest.exists() {
            if fs::copy(&src, &dest).is_err() {
                continue;
            }
        }
        let name = src.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Unknown".into());
        out.push(json!({ "id": id, "file": file, "ext": ext, "name": name }));
    }
    out
}

/// Read title/artist/album/duration and cover bytes using lofty.
pub fn read_tags(path: &PathBuf) -> (String, String, String, u64, Option<Vec<u8>>) {
    use lofty::prelude::*;
    let mut title = String::new();
    let mut artist = String::new();
    let mut album = String::new();
    let mut duration = 0u64;

    let probe = lofty::probe::Probe::open(path);
    let tagged = match probe.and_then(|p| p.read()) {
        Ok(t) => t,
        Err(_) => return (title, artist, album, duration, None),
    };
    duration = tagged.properties().duration().as_secs();
    for tag in tagged.tags().iter() {
        if title.is_empty() {
            if let Some(v) = tag.get_string(&lofty::tag::ItemKey::TrackTitle) {
                title = v.to_string();
            }
        }
        if artist.is_empty() {
            if let Some(v) = tag.get_string(&lofty::tag::ItemKey::TrackArtist) {
                artist = v.to_string();
            }
        }
        if album.is_empty() {
            if let Some(v) = tag.get_string(&lofty::tag::ItemKey::AlbumTitle) {
                album = v.to_string();
            }
        }
    }
    let cover = tagged
        .tags().iter()
        .filter_map(|tag| tag.pictures().first())
        .next()
        .map(|pic| pic.data().to_vec());
    (title, artist, album, duration, cover)
}

/// Merge imported items into library.json and enrich with tags read from disk.
pub fn add_to_library(items: &[Value]) {
    let mut tracks = load_library();
    for item in items {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        if id.is_empty() || tracks.iter().any(|t| t.get("id").and_then(|v| v.as_str()) == Some(id.as_str())) {
            continue;
        }
        let file = item.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut title = item.get("name").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
        let mut artist = String::new();
        let mut album = String::new();
        let mut duration = 0u64;
        if let Some(path) = find_track_by_id(&id) {
            let (t, a, al, d, cover) = read_tags(&path);
            if !t.is_empty() {
                title = t;
            }
            artist = a;
            album = al;
            duration = d;
            if let Some(bytes) = cover {
                let cover_path = util::covers_dir().join(format!("{id}.jpg"));
                let _ = fs::write(&cover_path, bytes);
            }
        }
        tracks.push(json!({
            "id": id,
            "file": file,
            "ext": item.get("ext").cloned().unwrap_or(json!("mp3")),
            "name": item.get("name").cloned().unwrap_or(json!(title.clone())),
            "title": title,
            "artist": artist,
            "album": album,
            "duration": duration,
            "cleanId": id,
            "platform": "local",
            "isLocal": true,
            "addedAt": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        }));
    }
    save_library(&tracks);
}

pub fn update_library_entry(id: &str, patch: &serde_json::Map<String, Value>) {
    let mut tracks = load_library();
    for track in tracks.iter_mut() {
        if track.get("id").and_then(|v| v.as_str()) == Some(id) {
            for (key, value) in patch {
                track[key] = value.clone();
            }
        }
    }
    save_library(&tracks);
}

pub fn remove_library_entry(id: &str) -> bool {
    let mut tracks = load_library();
    let before = tracks.len();
    tracks.retain(|t| t.get("id").and_then(|v| v.as_str()) != Some(id));
    save_library(&tracks);
    if let Some(path) = find_track_by_id(id) {
        let _ = fs::remove_file(path);
    }
    // cover thumbnail
    let _ = fs::remove_file(util::covers_dir().join(format!("{id}.jpg")));
    before != tracks.len()
}
