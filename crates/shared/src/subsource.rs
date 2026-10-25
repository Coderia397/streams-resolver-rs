//! SubSource subtitle search — REST client supporting both keyless and authenticated lookups.
//!
//! SubSource (subsource.net) hosts a repository of subtitle files for movies
//! and TV series. SubSource supports:
//! 1. Direct REST retrieval via its public endpoints (`/v1/movie/tmdb`,
//!    `/v1/subtitles/{slug}`, and `/v1/subtitle/{link}`).
//! 2. Authenticated REST lookups when `SUBSOURCE_API_KEY` is provided in the environment.
//!
//! Downloads are delivered as zip files from `api.subsource.net/v1/subtitle/download/{token}`
//! which require no authentication headers and are downloaded directly by the viewer.

use crate::http::{get_text_with, GIGA};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::Duration;

pub struct SearchArgs<'a> {
    pub tmdb_id: &'a str,
    pub is_tv: bool,
    pub season: u32,
    pub episode: u32,
    pub langs: &'a str,
}

const LIST_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const PER_LANGUAGE: usize = 3;
const API_BASE: &str = "https://api.subsource.net";

const HEADERS: &[(&str, &str)] = &[
    ("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"),
    ("Accept", "application/json, text/plain, */*"),
];

/// Maps 2-letter or common language codes into SubSource's canonical language names.
pub fn to_subsource_lang(code: &str) -> Option<&'static str> {
    match code.trim().to_ascii_lowercase().as_str() {
        "en" | "eng" | "english" => Some("english"),
        "ar" | "ara" | "arabic" => Some("arabic"),
        "es" | "spa" | "spanish" => Some("spanish"),
        "fr" | "fre" | "fra" | "french" => Some("french"),
        "de" | "ger" | "deu" | "german" => Some("german"),
        "it" | "ita" | "italian" => Some("italian"),
        "pt" | "por" | "br" | "pt_br" | "portuguese" => Some("portuguese"),
        "ru" | "rus" | "russian" => Some("russian"),
        "ja" | "jpn" | "japanese" => Some("japanese"),
        "ko" | "kor" | "korean" => Some("korean"),
        "zh" | "chi" | "zho" | "chinese" => Some("chinese"),
        "tr" | "tur" | "turkish" => Some("turkish"),
        "vi" | "vie" | "vietnamese" => Some("vietnamese"),
        "nl" | "dut" | "nld" | "dutch" => Some("dutch"),
        "pl" | "pol" | "polish" => Some("polish"),
        "sv" | "swe" | "swedish" => Some("swedish"),
        "id" | "ind" | "indonesian" => Some("indonesian"),
        "fa" | "per" | "fas" | "persian" => Some("persian"),
        "hi" | "hin" | "hindi" => Some("hindi"),
        "el" | "ell" | "greek" => Some("greek"),
        "cs" | "cze" | "ces" | "czech" => Some("czech"),
        "da" | "dan" | "danish" => Some("danish"),
        "fi" | "fin" | "finnish" => Some("finnish"),
        "hu" | "hun" | "hungarian" => Some("hungarian"),
        "no" | "nor" | "norwegian" => Some("norwegian"),
        "ro" | "rum" | "ron" | "romanian" => Some("romanian"),
        "th" | "tha" | "thai" => Some("thai"),
        "uk" | "ukr" | "ukrainian" => Some("ukrainian"),
        "he" | "heb" | "hebrew" => Some("hebrew"),
        _ => None,
    }
}

/// Maps SubSource lowercase language names into (standard uppercase code, display label).
pub fn from_subsource_lang(name: &str) -> (&'static str, &'static str) {
    match name.trim().to_ascii_lowercase().as_str() {
        "english" => ("EN", "English"),
        "arabic" => ("AR", "Arabic"),
        "spanish" => ("ES", "Spanish"),
        "french" => ("FR", "French"),
        "german" => ("DE", "German"),
        "italian" => ("IT", "Italian"),
        "portuguese" => ("PT", "Portuguese"),
        "russian" => ("RU", "Russian"),
        "japanese" => ("JA", "Japanese"),
        "korean" => ("KO", "Korean"),
        "chinese" => ("ZH", "Chinese"),
        "turkish" => ("TR", "Turkish"),
        "vietnamese" => ("VI", "Vietnamese"),
        "dutch" => ("NL", "Dutch"),
        "polish" => ("PL", "Polish"),
        "swedish" => ("SV", "Swedish"),
        "indonesian" => ("ID", "Indonesian"),
        "persian" => ("FA", "Persian"),
        "hindi" => ("HI", "Hindi"),
        "greek" => ("EL", "Greek"),
        "czech" => ("CS", "Czech"),
        "danish" => ("DA", "Danish"),
        "finnish" => ("FI", "Finnish"),
        "hungarian" => ("HU", "Hungarian"),
        "norwegian" => ("NO", "Norwegian"),
        "romanian" => ("RO", "Romanian"),
        "thai" => ("TH", "Thai"),
        "ukrainian" => ("UK", "Ukrainian"),
        "hebrew" => ("HE", "Hebrew"),
        _ => ("OTHER", "Other"),
    }
}

fn is_subtitle_file(filename: &str) -> bool {
    let lower = filename.to_ascii_lowercase();
    (lower.ends_with(".srt") || lower.ends_with(".vtt"))
        && !lower.starts_with("__macosx")
        && !lower.starts_with('.')
        && !lower.ends_with('/')
}

fn extract_via_central_directory(zip_bytes: &[u8]) -> Option<Vec<u8>> {
    let len = zip_bytes.len();
    if len < 22 {
        return None;
    }

    // Find EOCD marker b"PK\x05\x06" in last 65k bytes
    let min_search = len.saturating_sub(65536 + 22);
    let mut eocd_pos = None;
    for i in (min_search..=len - 22).rev() {
        if &zip_bytes[i..i + 4] == b"PK\x05\x06" {
            eocd_pos = Some(i);
            break;
        }
    }
    let eocd = eocd_pos?;
    let cd_size = u32::from_le_bytes(zip_bytes[eocd + 12..eocd + 16].try_into().ok()?) as usize;
    let cd_offset = u32::from_le_bytes(zip_bytes[eocd + 16..eocd + 20].try_into().ok()?) as usize;

    if cd_offset + cd_size > len {
        return None;
    }

    let mut cur = cd_offset;
    while cur + 46 <= cd_offset + cd_size {
        if &zip_bytes[cur..cur + 4] != b"PK\x01\x02" {
            break;
        }
        let comp_method = u16::from_le_bytes(zip_bytes[cur + 10..cur + 12].try_into().ok()?);
        let comp_size = u32::from_le_bytes(zip_bytes[cur + 20..cur + 24].try_into().ok()?) as usize;
        let fn_len = u16::from_le_bytes(zip_bytes[cur + 28..cur + 30].try_into().ok()?) as usize;
        let extra_len = u16::from_le_bytes(zip_bytes[cur + 30..cur + 32].try_into().ok()?) as usize;
        let comm_len = u16::from_le_bytes(zip_bytes[cur + 32..cur + 34].try_into().ok()?) as usize;
        let local_offset = u32::from_le_bytes(zip_bytes[cur + 42..cur + 46].try_into().ok()?) as usize;

        if cur + 46 + fn_len > len {
            break;
        }

        let filename = String::from_utf8_lossy(&zip_bytes[cur + 46..cur + 46 + fn_len]);
        if is_subtitle_file(&filename) {
            if local_offset + 30 <= len && &zip_bytes[local_offset..local_offset + 4] == b"PK\x03\x04" {
                let local_fn_len = u16::from_le_bytes(zip_bytes[local_offset + 26..local_offset + 28].try_into().ok()?) as usize;
                let local_extra_len = u16::from_le_bytes(zip_bytes[local_offset + 28..local_offset + 30].try_into().ok()?) as usize;
                let data_start = local_offset + 30 + local_fn_len + local_extra_len;
                let data_end = data_start + comp_size;
                if data_end <= len {
                    let decompressed = match comp_method {
                        0 => zip_bytes[data_start..data_end].to_vec(),
                        8 => miniz_oxide::inflate::decompress_to_vec(&zip_bytes[data_start..data_end]).ok()?,
                        _ => return None,
                    };
                    return Some(decompressed);
                }
            }
        }

        cur += 46 + fn_len + extra_len + comm_len;
    }

    None
}

fn extract_via_local_headers(zip_bytes: &[u8]) -> Option<Vec<u8>> {
    let mut cursor = 0;
    let len = zip_bytes.len();

    while cursor + 30 <= len {
        if &zip_bytes[cursor..cursor + 4] != b"PK\x03\x04" {
            break;
        }

        let comp_method = u16::from_le_bytes(zip_bytes[cursor + 8..cursor + 10].try_into().ok()?);
        let comp_size = u32::from_le_bytes(zip_bytes[cursor + 18..cursor + 22].try_into().ok()?) as usize;
        let fn_len = u16::from_le_bytes(zip_bytes[cursor + 26..cursor + 28].try_into().ok()?) as usize;
        let extra_len = u16::from_le_bytes(zip_bytes[cursor + 28..cursor + 30].try_into().ok()?) as usize;

        let data_start = cursor + 30 + fn_len + extra_len;
        if data_start > len {
            break;
        }

        let filename = if cursor + 30 + fn_len <= len {
            String::from_utf8_lossy(&zip_bytes[cursor + 30..cursor + 30 + fn_len]).to_string()
        } else {
            String::new()
        };

        if is_subtitle_file(&filename) {
            let compressed_slice = if comp_size > 0 && data_start + comp_size <= len {
                &zip_bytes[data_start..data_start + comp_size]
            } else {
                &zip_bytes[data_start..]
            };

            let decompressed = match comp_method {
                0 => compressed_slice.to_vec(),
                8 => miniz_oxide::inflate::decompress_to_vec(compressed_slice).ok()?,
                _ => return None,
            };
            return Some(decompressed);
        }

        if comp_size > 0 && data_start + comp_size <= len {
            cursor = data_start + comp_size;
        } else {
            break;
        }
    }

    None
}

/// Extracts raw bytes of the first .srt or .vtt subtitle file from a raw PK zip archive.
/// Inspects filenames to skip readme files, directories, and non-subtitle entries.
/// Returns raw bytes to preserve non-UTF-8 character encodings (e.g. Windows-1256 Arabic, Windows-1251 Cyrillic).
pub fn extract_srt_from_zip(zip_bytes: &[u8]) -> Option<Vec<u8>> {
    if zip_bytes.len() < 30 || &zip_bytes[0..4] != b"PK\x03\x04" {
        return None;
    }

    if let Some(bytes) = extract_via_central_directory(zip_bytes) {
        return Some(bytes);
    }

    extract_via_local_headers(zip_bytes)
}

/// Strict TV episode matcher: verifies season and episode tokens with word boundaries.
/// Prevents false positives where episode 1 matches episodes 11 to 19.
pub fn matches_episode_strict(text: &str, season: u32, episode: u32) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }

    let s_re = match regex::Regex::new(r"(?i)(?:^|[^a-z0-9])s(?:eason)?\s*0*(\d+)") {
        Ok(r) => r,
        Err(_) => return false,
    };
    let x_re = match regex::Regex::new(r"(?i)(?:^|[^a-z0-9])0*(\d+)\s*x") {
        Ok(r) => r,
        Err(_) => return false,
    };

    let mentioned_season = s_re.captures(text)
        .or_else(|| x_re.captures(text))
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<u32>().ok());

    if let Some(s) = mentioned_season {
        if s != season {
            return false;
        }
    }

    let p1 = format!(r"(?i)(?:^|[^a-z0-9])s(?:eason)?[\s\.\-_]*0*{season}[\.\-_\s]*(?:e|ep|episode)[\s\.\-_]*0*{episode}(?:[^0-9]|$)");
    let p2 = format!(r"(?i)(?:^|[^a-z0-9])0*{season}x0*{episode}(?:[^0-9]|$)");
    let p3 = format!(r"(?i)(?:^|[^a-z0-9])(?:e|ep|episode)[\s\.\-_]*0*{episode}(?:[^0-9]|$)");

    regex::Regex::new(&p1).map(|r| r.is_match(text)).unwrap_or(false)
        || regex::Regex::new(&p2).map(|r| r.is_match(text)).unwrap_or(false)
        || regex::Regex::new(&p3).map(|r| r.is_match(text)).unwrap_or(false)
}

pub async fn search(args: SearchArgs<'_>) -> Value {
    let langs_clean: String = args.langs
        .to_ascii_uppercase()
        .chars()
        .filter(|c| c.is_ascii_alphabetic() || *c == '_' || *c == ',')
        .take(160)
        .collect();

    let cache_key = format!(
        "subsource:{}:{}:{}:{}:{}",
        args.tmdb_id, args.is_tv, args.season, args.episode, langs_clean
    );

    if let Some(hit) = crate::cache::get(&cache_key) {
        return hit;
    }

    // Parse requested languages filter
    let requested_targets: HashSet<&'static str> = langs_clean
        .split(',')
        .filter_map(to_subsource_lang)
        .collect();

    // Step 1: Query TMDB metadata on SubSource to obtain IMDb ID and title
    let tmdb_lookup_url = format!("{API_BASE}/v1/movie/tmdb");
    let kind_path = if args.is_tv { "tv" } else { "movie" };
    let tmdb_link = format!("https://www.themoviedb.org/{}/{}", kind_path, args.tmdb_id);
    let payload = json!({ "link": tmdb_link });

    let resp_res = GIGA
        .post(&tmdb_lookup_url)
        .json(&payload)
        .timeout(Duration::from_secs(8))
        .send()
        .await;

    let meta_json: Value = match resp_res {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
        _ => Value::Null,
    };

    let imdb_id = meta_json.get("imdbId").and_then(Value::as_str).unwrap_or("");
    let title = meta_json.get("title").and_then(Value::as_str).unwrap_or("");

    // Step 2: Search SubSource catalog using IMDb ID (exact direct match) or title
    let search_url = format!("{API_BASE}/v1/movie/search");
    let mut search_query = if !imdb_id.is_empty() { imdb_id } else { title };
    if search_query.is_empty() {
        search_query = args.tmdb_id;
    }

    let search_res = GIGA
        .post(&search_url)
        .json(&json!({ "query": search_query }))
        .timeout(Duration::from_secs(8))
        .send()
        .await;

    let mut search_json: Value = match search_res {
        Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
        _ => Value::Null,
    };

    // If search by IMDb ID yielded no results and title is available, retry by title
    let has_results = search_json.get("results").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
    if !has_results && search_query == imdb_id && !title.is_empty() {
        let retry_res = GIGA
            .post(&search_url)
            .json(&json!({ "query": title }))
            .timeout(Duration::from_secs(8))
            .send()
            .await;
        if let Ok(r) = retry_res {
            if r.status().is_success() {
                search_json = r.json().await.unwrap_or(Value::Null);
            }
        }
    }

    let results = match search_json.get("results").and_then(Value::as_array) {
        Some(arr) if !arr.is_empty() => arr,
        _ => return json!({ "subtitles": [] }),
    };

    // Match candidate item based on media kind
    let target_item = if args.is_tv {
        results.iter().find(|i| {
            let t = i.get("type").and_then(Value::as_str).unwrap_or("");
            t == "tvseries" || t == "series" || t == "tv"
        }).or_else(|| results.first())
    } else {
        results.iter().find(|i| {
            let t = i.get("type").and_then(Value::as_str).unwrap_or("");
            t == "movie"
        }).or_else(|| results.first())
    };

    let raw_link = target_item
        .and_then(|i| i.get("link"))
        .and_then(Value::as_str)
        .unwrap_or("");

    let slug = raw_link
        .trim_start_matches("/series/")
        .trim_start_matches("/subtitles/")
        .trim_matches('/');

    if slug.is_empty() {
        return json!({ "subtitles": [] });
    }

    // Step 3: Fetch list of available subtitles for movie or TV season
    let list_url = if args.is_tv {
        format!("{API_BASE}/v1/subtitles/{slug}/season-{}", args.season)
    } else {
        format!("{API_BASE}/v1/subtitles/{slug}")
    };

    let list_body = match get_text_with(&GIGA, &list_url, Duration::from_secs(8), HEADERS).await {
        Some(b) => b,
        None if args.is_tv => {
            let fallback_url = format!("{API_BASE}/v1/subtitles/{slug}");
            match get_text_with(&GIGA, &fallback_url, Duration::from_secs(8), HEADERS).await {
                Some(b) => b,
                None => return json!({ "subtitles": [] }),
            }
        }
        None => return json!({ "subtitles": [] }),
    };

    let list_data: Value = serde_json::from_str(&list_body).unwrap_or(Value::Null);
    let raw_subs = list_data.get("subtitles").and_then(Value::as_array).cloned().unwrap_or_default();

    // Step 4: Filter by language and TV episode
    let mut chosen_candidates: Vec<Value> = Vec::new();
    let mut count_per_lang: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    // Sort hearing_impaired == 0 first
    let mut sorted_subs = raw_subs;
    sorted_subs.sort_by_key(|s| s.get("hearing_impaired").and_then(Value::as_u64).unwrap_or(0));

    for s in &sorted_subs {
        let lang = s.get("language").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
        if lang.is_empty() {
            continue;
        }

        // If specific languages requested, skip non-matching
        if !requested_targets.is_empty() && !requested_targets.contains(lang.as_str()) {
            continue;
        }

        // For TV shows, verify episode if present
        if args.is_tv {
            if let Some(ep) = s.get("episode").and_then(Value::as_u64) {
                if ep != args.episode as u64 {
                    continue;
                }
            } else {
                let rel = s.get("release_info").and_then(Value::as_str).unwrap_or("");
                let link = s.get("link").and_then(Value::as_str).unwrap_or("");
                if !matches_episode_strict(rel, args.season, args.episode)
                    && !matches_episode_strict(link, args.season, args.episode)
                {
                    continue;
                }
            }
        }

        let current_count = count_per_lang.entry(lang.clone()).or_insert(0);
        if *current_count >= PER_LANGUAGE {
            continue;
        }

        *current_count += 1;
        chosen_candidates.push(s.clone());
    }

    // Step 5: For each selected candidate, fetch download token concurrently
    let download_futs = chosen_candidates.into_iter().map(|s| async move {
        let link = s.get("link").and_then(Value::as_str).unwrap_or("").trim_matches('/');
        if link.is_empty() {
            return None;
        }

        let detail_url = format!("{API_BASE}/v1/subtitle/{link}");
        let detail_body = get_text_with(&GIGA, &detail_url, Duration::from_secs(6), HEADERS).await?;
        let detail_json: Value = serde_json::from_str(&detail_body).ok()?;
        let token = detail_json
            .get("subtitle")
            .and_then(|sub| sub.get("download_token"))
            .and_then(Value::as_str)?;

        let lang_raw = s.get("language").and_then(Value::as_str).unwrap_or("");
        let (lang_code, lang_label) = from_subsource_lang(lang_raw);
        let hi = s.get("hearing_impaired").and_then(Value::as_u64).unwrap_or(0) == 1;
        let release = s.get("release_info").and_then(Value::as_str).unwrap_or("").to_string();

        Some(json!({
            "language": lang_code,
            "label": lang_label,
            "hi": hi,
            "release": release,
            "format": "srt",
            "fps": null,
            "url": format!("https://resolver.streams.watch/api/subtitles/download?token={token}"),
            "zip": false,
        }))
    });

    let results = futures::future::join_all(download_futs).await;
    let final_subtitles: Vec<Value> = results.into_iter().flatten().collect();

    let out = json!({ "subtitles": final_subtitles });
    crate::cache::put(cache_key, out.clone(), LIST_TTL);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_language_codes_accurately() {
        assert_eq!(to_subsource_lang("en"), Some("english"));
        assert_eq!(to_subsource_lang("EN"), Some("english"));
        assert_eq!(to_subsource_lang("ar"), Some("arabic"));
        assert_eq!(to_subsource_lang("es"), Some("spanish"));
        assert_eq!(to_subsource_lang("unknown_lang"), None);

        let (code, label) = from_subsource_lang("english");
        assert_eq!(code, "EN");
        assert_eq!(label, "English");

        let (code, label) = from_subsource_lang("arabic");
        assert_eq!(code, "AR");
        assert_eq!(label, "Arabic");
    }

    #[test]
    fn strictly_matches_target_episode() {
        assert!(matches_episode_strict("Breaking.Bad.S01E01.720p", 1, 1));
        assert!(matches_episode_strict("Breaking.Bad.S1E1.720p", 1, 1));
        assert!(matches_episode_strict("Breaking.Bad.1x01.Pilot", 1, 1));
        assert!(matches_episode_strict("breaking-bad-episode-1", 1, 1));

        // Must reject false positives E11 - E19
        assert!(!matches_episode_strict("Breaking.Bad.S01E11.720p", 1, 1));
        assert!(!matches_episode_strict("Breaking.Bad.S01E12.720p", 1, 1));
        assert!(!matches_episode_strict("Breaking.Bad.S01E19.720p", 1, 1));
        assert!(!matches_episode_strict("Breaking.Bad.S01E10.720p", 1, 1));

        // Must reject mismatched season
        assert!(!matches_episode_strict("Breaking.Bad.S02E01.720p", 1, 1));
        assert!(!matches_episode_strict("Breaking.Bad.2x01.Pilot", 1, 1));
    }

    #[tokio::test]
    #[ignore]
    async fn live_subsource_movie_lookup() {
        let res = search(SearchArgs {
            tmdb_id: "27205",
            is_tv: false,
            season: 1,
            episode: 1,
            langs: "EN,AR",
        }).await;

        let subs = res.get("subtitles").and_then(Value::as_array).unwrap();
        assert!(!subs.is_empty());
        assert!(subs[0].get("url").and_then(Value::as_str).unwrap().contains("/api/subtitles/download?token="));
    }

    #[tokio::test]
    #[ignore]
    async fn live_subsource_tv_lookup() {
        let res = search(SearchArgs {
            tmdb_id: "1396",
            is_tv: true,
            season: 1,
            episode: 1,
            langs: "EN",
        }).await;

        let subs = res.get("subtitles").and_then(Value::as_array).unwrap();
        assert!(!subs.is_empty());
    }
}
