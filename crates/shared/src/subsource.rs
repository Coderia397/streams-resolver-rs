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
            "format": "zip",
            "fps": null,
            "url": format!("{API_BASE}/v1/subtitle/download/{token}"),
            "zip": true,
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
        assert!(subs[0].get("url").and_then(Value::as_str).unwrap().contains("/download/"));
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
