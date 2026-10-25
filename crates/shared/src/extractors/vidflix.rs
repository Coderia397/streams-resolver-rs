use crate::http::{get_text_with, GIGA, PROXY};
use crate::models::{MediaKind, ProviderResult, Source};
use crate::utils::sort_sources_by_quality;
use serde_json::Value;
use std::time::Duration;

pub const ID: &str = "vidflix";
pub const NAME: &str = "Vidflix ⚡";

const PRIMARY_BASE: &str = "https://player.vidflix.club";
const PRIMARY_KEY: &str = "f3b72e73c80c9a996574379798703796a1936efa3516a7105cb0e43048b46b5a";

const HEADERS_PRIMARY: &[(&str, &str)] = &[
    ("x-player-key", PRIMARY_KEY),
    ("Referer", "https://player.vidflix.club/"),
    ("Origin", "https://player.vidflix.club"),
    ("Accept", "application/json, text/plain, */*"),
    ("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"),
];

/// Extracts and normalizes playable HLS sources from Vidflix API response without duplicates.
pub fn parse_sources(json_data: &Value, referer: &str) -> Vec<Source> {
    let mut sources = Vec::new();
    let Some(raw_sources) = json_data.get("sources").and_then(Value::as_array) else {
        return sources;
    };

    for item in raw_sources {
        let Some(stream_url) = item.get("url").and_then(Value::as_str) else {
            continue;
        };
        let stream_url = stream_url.trim();
        if stream_url.is_empty() {
            continue;
        }

        let quality = if stream_url.contains("/2160") || stream_url.contains("4k") {
            "2160p"
        } else if stream_url.contains("/1080") {
            "1080p"
        } else if stream_url.contains("/720") {
            "720p"
        } else if stream_url.contains("/480") {
            "480p"
        } else {
            "auto"
        };

        // Emit exactly one source per unique stream URL
        let mut s = Source::direct_m3u8(stream_url, quality).tagged(NAME, ID);
        s = s.with_referer(referer).not_embed();
        sources.push(s);
    }

    sort_sources_by_quality(&mut sources);
    sources
}

pub async fn scrape(
    tmdb_id: &str,
    kind: MediaKind,
    season: u32,
    episode: u32,
    _title: Option<&str>,
    _year: Option<u32>,
) -> Option<ProviderResult> {
    let path = if kind.is_movie() {
        format!("/api/vidora/v1/movie/{tmdb_id}")
    } else {
        format!("/api/vidora/v1/tv/{tmdb_id}/{season}/{episode}")
    };

    let primary_url = format!("{PRIMARY_BASE}{path}");
    let resp = match get_text_with(&GIGA, &primary_url, Duration::from_secs(6), HEADERS_PRIMARY).await {
        Some(b) => b,
        None => get_text_with(&PROXY, &primary_url, Duration::from_secs(8), HEADERS_PRIMARY).await?,
    };

    let json_val: Value = serde_json::from_str(&resp).ok()?;
    if json_val.get("result").and_then(Value::as_bool) != Some(true) {
        return None;
    }

    let mut sources = parse_sources(&json_val, PRIMARY_BASE);

    // Provide clean player embed fallback that works in browser iframes
    let embed_url = if kind.is_movie() {
        format!("{PRIMARY_BASE}/movie/{tmdb_id}")
    } else {
        format!("{PRIMARY_BASE}/tv/{tmdb_id}/{season}/{episode}")
    };
    sources.push(Source::embed(embed_url, "auto").tagged(NAME, ID).with_referer(PRIMARY_BASE));

    ProviderResult::some_if_any(NAME, ID, sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOCK_RESPONSE: &str = r#"{
        "result": true,
        "type": "movie",
        "title": "Inception",
        "tmdb_id": "27205",
        "imdb_id": "tt1375666",
        "year": 2010,
        "sources": [
            {
                "file_code": "27205",
                "url": "https://dn6jx3q12wgk.johnkulumbe.workers.dev/?url=https%3A%2F%2Fnetoda.tech%2Fhls%2Fmaster.m3u8&data=test&type=/index.m3u8",
                "source": "k2mq"
            }
        ]
    }"#;

    #[test]
    fn parses_vidflix_sources_successfully() {
        let val: Value = serde_json::from_str(MOCK_RESPONSE).unwrap();
        let sources = parse_sources(&val, PRIMARY_BASE);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].quality, "auto");
        assert!(sources[0].is_direct());
        assert_eq!(sources[0].referer.as_deref(), Some(PRIMARY_BASE));
    }

    #[tokio::test]
    #[ignore]
    async fn live_vidflix_movie_scrape() {
        let res = scrape("27205", MediaKind::Movie, 1, 1, Some("Inception"), Some(2010)).await.unwrap();
        assert!(!res.sources.is_empty());
        assert!(res.sources.iter().any(|s| s.is_direct()));
    }

    #[tokio::test]
    #[ignore]
    async fn live_vidflix_tv_scrape() {
        let res = scrape("1396", MediaKind::Tv, 1, 1, Some("Breaking Bad"), Some(2008)).await.unwrap();
        assert!(!res.sources.is_empty());
        assert!(res.sources.iter().any(|s| s.is_direct()));
    }
}
