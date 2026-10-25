//! SubDL subtitle search — port of `subdlSearch` in `local-resolver/server.mjs`.
//!
//! Same reasoning as the YouTube endpoint: the frontend used to call SubDL
//! directly with the key in the bundle, where anyone could read it. Only the
//! *search* needs the key — the subtitle files SubDL returns are plain public
//! URLs the browser fetches itself (dl.subdl.com answers any site, 300 files a
//! day per viewer) — so just this one call is proxied, the key never leaves the
//! device, and no subtitle file passes through the phone.
//!
//! Set `SUBDL_API_KEY` (no `VITE_` prefix) in the environment to enable.

use crate::http::{get_text_with, GIGA};
use serde_json::{json, Value};
use std::time::Duration;

pub struct SearchArgs<'a> {
    pub tmdb_id: &'a str,
    pub is_tv: bool,
    pub season: u32,
    pub episode: u32,
    pub langs: &'a str,
}

/// How long a title's subtitle list is kept (the free key allows 2,000 searches a day).
const LIST_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// Candidates kept per language: the first is used; the others are there when it does not match the video's timing.
const PER_LANGUAGE: usize = 3;

/// The subtitle list of a title, one entry per file the browser can download itself (dl.subdl.com answers any site):
/// `{ subtitles: [ { language, label, hi, release, format, fps, url, zip } ] }`, at most three per language, files that are not for
/// hearing impaired viewers first. `url` is a single subtitle file when SubDL has unpacked it, otherwise the zip (`zip: true`).
/// The same shape with an `error` when the lookup could not be made.
pub async fn search(args: SearchArgs<'_>) -> Value {
    let Ok(key) = std::env::var("SUBDL_API_KEY") else {
        return json!({ "subtitles": [], "error": "SUBDL_API_KEY not configured" });
    };
    if key.is_empty() {
        return json!({ "subtitles": [], "error": "SUBDL_API_KEY not configured" });
    }
    // language codes as SubDL writes them (EN, FR, BR_PT ...): letters, underscores and commas only
    let langs: String = args.langs.to_ascii_uppercase().chars().filter(|c| c.is_ascii_alphabetic() || *c == '_' || *c == ',').take(160).collect();
    let cache_key = format!("subdl:{}:{}:{}:{}:{}", args.tmdb_id, args.is_tv, args.season, args.episode, langs);
    if let Some(hit) = crate::cache::get(&cache_key) {
        return hit;
    }
    let media_type = if args.is_tv { "tv" } else { "movie" };
    // `languages` (not `language`, which SubDL ignores and which returned 30 subtitles in any language before 2026-10-25)
    let mut url = format!(
        "https://api.subdl.com/api/v1/subtitles?api_key={}&tmdb_id={}&type={}&subs_per_page=30&unpack=1&client=custom_integration",
        urlencoding::encode(&key),
        urlencoding::encode(args.tmdb_id),
        media_type,
    );
    if !langs.is_empty() {
        url.push_str(&format!("&languages={}", urlencoding::encode(&langs)));
    }
    if args.is_tv {
        url.push_str(&format!("&season_number={}&episode_number={}", args.season, args.episode));
    }
    let Some(body) = get_text_with(&GIGA, &url, Duration::from_secs(10), &[("Accept", "application/json")]).await else {
        return json!({ "subtitles": [], "error": "SubDL request failed" });
    };
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return json!({ "subtitles": [], "error": "SubDL returned invalid JSON" }),
    };
    let raw = parsed.get("subtitles").and_then(Value::as_array).cloned().unwrap_or_default();
    let out = json!({ "subtitles": normalise(&raw, args.is_tv, args.season, args.episode) });
    crate::cache::put(cache_key, out.clone(), LIST_TTL);
    out
}

/// SubDL's list as the player wants it (see `search`).
pub fn normalise(raw: &[Value], is_tv: bool, season: u32, episode: u32) -> Vec<Value> {
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    // SubDL writes the API key into the file addresses it returns (`?api_key=...`); downloads work without it (300 a day per viewer),
    // so everything after `?` is dropped and the key never reaches a browser
    let path_of = |v: &Value| text(v, "url").split('?').next().unwrap_or("").to_string();
    let mut found: Vec<(String, Value)> = Vec::new();
    for s in raw {
        let language = text(s, "language").to_ascii_uppercase();
        if language.is_empty() {
            continue;
        }
        let label = text(s, "lang");
        let fps = s.get("fps").cloned().unwrap_or(Value::Null);
        let files = s.get("unpack_files").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut added = false;
        for f in &files {
            let path = path_of(f);
            let format = text(f, "format").to_ascii_lowercase();
            if path.is_empty() || !matches!(format.as_str(), "srt" | "vtt" | "") {
                continue;
            }
            // a season pack holds every episode: keep only this one
            if is_tv {
                let fs = f.get("season").and_then(Value::as_u64);
                let fe = f.get("episode").and_then(Value::as_u64);
                if fs.is_some_and(|x| x != season as u64) || fe.is_some_and(|x| x != episode as u64) {
                    continue;
                }
            }
            let lang = Some(text(f, "language").to_ascii_uppercase()).filter(|l| !l.is_empty()).unwrap_or_else(|| language.clone());
            found.push((lang.clone(), json!({
                "language": lang, "label": label, "hi": f.get("hi").and_then(Value::as_bool).unwrap_or(false),
                "release": text(f, "release_name"), "format": if format.is_empty() { "srt".to_string() } else { format },
                "fps": fps, "url": format!("https://dl.subdl.com{path}"), "zip": false,
            })));
            added = true;
        }
        let path = path_of(s);
        if !added && !path.is_empty() && !(is_tv && s.get("full_season").and_then(Value::as_bool).unwrap_or(false)) {
            found.push((language.clone(), json!({
                "language": language, "label": label, "hi": s.get("hi").and_then(Value::as_bool).unwrap_or(false),
                "release": text(s, "release_name"), "format": "zip", "fps": fps, "url": format!("https://dl.subdl.com{path}"), "zip": true,
            })));
        }
    }
    // per language: single files before zips, then not hearing impaired first, in SubDL's own order otherwise; three at most
    found.sort_by_key(|(_, v)| (v["zip"].as_bool().unwrap_or(true), v["hi"].as_bool().unwrap_or(false)));
    let mut count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut by_lang: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    for (lang, v) in found {
        let n = count.entry(lang.clone()).or_insert(0);
        if *n >= PER_LANGUAGE {
            continue;
        }
        *n += 1;
        if !by_lang.contains_key(&lang) {
            order.push(lang.clone());
        }
        by_lang.entry(lang).or_default().push(v);
    }
    order.sort();
    order.into_iter().flat_map(|l| by_lang.remove(&l).unwrap_or_default()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_files_of_this_episode_and_three_per_language() {
        let raw = vec![
            json!({ "language": "EN", "lang": "English", "url": "/subtitle/1-1.zip", "full_season": true, "hi": false,
                    "unpack_files": [ { "url": "/subtitle/1/a", "season": 1, "episode": 1, "format": "srt", "hi": false, "release_name": "S01E01" },
                                      { "url": "/subtitle/1/b?api_key=secret", "season": 1, "episode": 2, "format": "srt", "hi": false } ] }),
            json!({ "language": "EN", "lang": "English", "url": "/subtitle/2-2.zip", "hi": true }),
            json!({ "language": "EN", "lang": "English", "url": "/subtitle/3-3.zip?api_key=secret", "hi": false }),
            json!({ "language": "EN", "lang": "English", "url": "/subtitle/4-4.zip", "hi": false }),
            json!({ "language": "FR", "lang": "French", "url": "/subtitle/5-5.zip", "hi": false }),
        ];
        let out = normalise(&raw, true, 1, 1);
        let urls: Vec<&str> = out.iter().map(|v| v["url"].as_str().unwrap()).collect();
        assert_eq!(urls, ["https://dl.subdl.com/subtitle/1/a", "https://dl.subdl.com/subtitle/3-3.zip", "https://dl.subdl.com/subtitle/4-4.zip", "https://dl.subdl.com/subtitle/5-5.zip"]);
        assert_eq!(out[0]["zip"], false);
        assert!(out.iter().all(|v| !v["url"].as_str().unwrap().contains("api_key")));
    }
}
