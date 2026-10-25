use crate::http::{get_text_with, GIGA, PROXY};
use crate::models::{MediaKind, ProviderResult, Source};
use crate::utils::sort_sources_by_quality;
use std::time::Duration;

pub const ID: &str = "cineby";
pub const NAME: &str = "Cineby 🍿";
pub const BASE: &str = "https://cno.to";

/// Headers required by cno.to's Cloudflare edge and video CDNs.
const HEADERS: &[(&str, &str)] = &[
    ("Referer", "https://cno.to/"),
    ("Accept", "*/*"),
    ("Accept-Language", "en-US,en;q=0.9"),
];

/// Parses an HLS master playlist into discrete `Source` instances labeled by resolution.
///
/// Master manifests from cno.to provide multi-bitrate streams:
/// ```text
/// #EXTM3U
/// #EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=16186248,RESOLUTION=3840x2160,...
/// https://v.lumenbox.top/v/.../3840x2160/index.m3u8
/// ```
pub fn parse_master_m3u8(body: &str, master_url: &str) -> Vec<Source> {
    let mut sources = Vec::new();
    let mut pending_quality: Option<String> = None;

    // Master playlist kept first as adaptive "auto" so mobile players adapt to connection speed
    if body.trim_start().starts_with("#EXTM3U") {
        let mut auto_master = Source::direct_m3u8(master_url, "auto").tagged(NAME, ID);
        auto_master = auto_master.with_referer(BASE).not_embed();
        sources.push(auto_master);
    }

    let mut variant_sources = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with("#EXT-X-STREAM-INF:") {
            // Extract RESOLUTION=(\d+)x(\d+)
            let quality = if let Some(idx) = trimmed.find("RESOLUTION=") {
                let rest = &trimmed[idx + 11..];
                let dim = rest.split(',').next().unwrap_or("");
                let h = dim.split('x').nth(1).and_then(|h| h.parse::<u32>().ok()).unwrap_or(0);
                match h {
                    h if h >= 2160 => "2160p".to_string(),
                    h if h >= 1440 => "1440p".to_string(),
                    h if h >= 1080 => "1080p".to_string(),
                    h if h >= 720 => "720p".to_string(),
                    h if h >= 480 => "480p".to_string(),
                    h if h > 0 => format!("{h}p"),
                    _ => "auto".to_string(),
                }
            } else {
                "auto".to_string()
            };
            pending_quality = Some(quality);
        } else if !trimmed.starts_with('#') {
            if let Some(quality) = pending_quality.take() {
                let stream_url = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
                    trimmed.to_string()
                } else if trimmed.starts_with('/') {
                    format!("{BASE}{trimmed}")
                } else {
                    // Resolve relative path against master playlist URL
                    match reqwest::Url::parse(master_url).and_then(|base| base.join(trimmed)) {
                        Ok(resolved) => resolved.to_string(),
                        Err(_) => format!("{BASE}/{trimmed}"),
                    }
                };

                let mut s = Source::direct_m3u8(stream_url, quality)
                    .tagged(NAME, ID);
                s = s.with_referer(BASE).not_embed();
                variant_sources.push(s);
            }
        }
    }

    sort_sources_by_quality(&mut variant_sources);
    sources.extend(variant_sources);
    sources
}

/// Fallback metadata scraping when direct Server 21 endpoint is not present.
///
/// Queries `/api/movie/{id}` or `/api/tv/{id}/episode/{s}/{e}` and extracts
/// playable entries from the `sources` array.
async fn scrape_metadata_fallback(
    tmdb_id: &str,
    kind: MediaKind,
    season: u32,
    episode: u32,
) -> Option<Vec<Source>> {
    let meta_url = if kind.is_movie() {
        format!("{BASE}/api/movie/{tmdb_id}")
    } else {
        format!("{BASE}/api/tv/{tmdb_id}/episode/{season}/{episode}")
    };

    let body = get_text_with(&GIGA, &meta_url, Duration::from_secs(6), HEADERS).await?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    let items = json.get("sources")?.as_array()?;

    let mut sources = Vec::new();
    for item in items {
        let Some(raw_url) = item.get("url").and_then(|v| v.as_str()) else {
            continue;
        };
        let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let label = item.get("label").and_then(|v| v.as_str()).unwrap_or("auto");

        let resolved_url = if raw_url.starts_with('/') {
            format!("{BASE}{raw_url}")
        } else {
            raw_url.to_string()
        };

        if kind == "hls" {
            // Attempt to fetch and parse the master manifest
            if let Some(m3u8_body) = get_text_with(&GIGA, &resolved_url, Duration::from_secs(6), HEADERS).await {
                let parsed = parse_master_m3u8(&m3u8_body, &resolved_url);
                if !parsed.is_empty() {
                    sources.extend(parsed);
                    continue;
                }
            }
            let mut s = Source::direct_m3u8(resolved_url, label).tagged(NAME, ID);
            s = s.with_referer(BASE).not_embed();
            sources.push(s);
        } else if kind == "embed" {
            let s = Source::embed(resolved_url, label).tagged(NAME, ID).with_referer(BASE);
            sources.push(s);
        }
    }

    if sources.is_empty() {
        None
    } else {
        sort_sources_by_quality(&mut sources);
        Some(sources)
    }
}

pub async fn scrape(
    tmdb_id: &str,
    kind: MediaKind,
    season: u32,
    episode: u32,
    _title: Option<&str>,
    _year: Option<u32>,
) -> Option<ProviderResult> {
    // Primary path: Server 21 provides direct master HLS manifest with up to 4K resolutions
    let server21_url = if kind.is_movie() {
        format!("{BASE}/api/s/21/movie/{tmdb_id}?v=3")
    } else {
        format!("{BASE}/api/s/21/tv/{tmdb_id}/{season}/{episode}?v=3")
    };

    let manifest_body = match get_text_with(&GIGA, &server21_url, Duration::from_secs(8), HEADERS).await {
        Some(b) => Some(b),
        None => get_text_with(&PROXY, &server21_url, Duration::from_secs(10), HEADERS).await,
    };

    if let Some(body) = manifest_body {
        if body.trim_start().starts_with("#EXTM3U") {
            let sources = parse_master_m3u8(&body, &server21_url);
            if !sources.is_empty() {
                return ProviderResult::some_if_any(NAME, ID, sources);
            }
        }
    }

    // Secondary path: Parse metadata fallback
    let fallback_sources = scrape_metadata_fallback(tmdb_id, kind, season, episode).await?;
    ProviderResult::some_if_any(NAME, ID, fallback_sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOCK_MASTER: &str = r#"#EXTM3U
#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=16186248,RESOLUTION=3840x2160,FRAME-RATE=23.974,CODECS="avc1.640033,mp4a.40.2",VIDEO-RANGE=SDR
https://v.lumenbox.top/v/test/3840x2160/index.m3u8
#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=6152556,RESOLUTION=1920x1080,FRAME-RATE=23.974,CODECS="avc1.640028,mp4a.40.2",VIDEO-RANGE=SDR
https://v.lumenbox.top/v/test/1920x1080/index.m3u8
#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=3121132,RESOLUTION=1280x720,FRAME-RATE=23.974,CODECS="avc1.4d401f,mp4a.40.2",VIDEO-RANGE=SDR
https://v.lumenbox.top/v/test/1280x720/index.m3u8
#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=1322438,RESOLUTION=852x480,FRAME-RATE=23.974,CODECS="avc1.4d401e,mp4a.40.2",VIDEO-RANGE=SDR
https://v.lumenbox.top/v/test/852x480/index.m3u8
"#;

    #[test]
    fn parses_master_m3u8_variants_in_descending_quality() {
        let master_url = "https://cno.to/api/s/21/movie/27205?v=3";
        let sources = parse_master_m3u8(MOCK_MASTER, master_url);
        assert_eq!(sources.len(), 5);
        assert_eq!(sources[0].quality, "auto");
        assert_eq!(sources[0].url, master_url);
        assert_eq!(sources[0].provider_id.as_deref(), Some("cineby"));
        assert!(sources[0].is_direct());
        assert_eq!(sources[0].referer.as_deref(), Some("https://cno.to"));

        assert_eq!(sources[1].quality, "2160p");
        assert_eq!(sources[1].url, "https://v.lumenbox.top/v/test/3840x2160/index.m3u8");
        assert_eq!(sources[2].quality, "1080p");
        assert_eq!(sources[3].quality, "720p");
        assert_eq!(sources[4].quality, "480p");
    }

    #[test]
    fn handles_single_manifest_fallback() {
        let simple_m3u8 = "#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nseg1.ts\n";
        let sources = parse_master_m3u8(simple_m3u8, "https://cno.to/stream.m3u8");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].quality, "auto");
        assert_eq!(sources[0].url, "https://cno.to/stream.m3u8");
    }

    #[test]
    fn handles_empty_or_garbage_manifest() {
        let sources = parse_master_m3u8("Not a playlist", "https://cno.to/invalid");
        assert!(sources.is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn live_cno_movie_scrape() {
        let res = scrape("27205", MediaKind::Movie, 1, 1, Some("Inception"), Some(2010)).await.unwrap();
        assert!(!res.sources.is_empty());
        assert!(res.sources[0].is_direct());
    }

    #[tokio::test]
    #[ignore]
    async fn live_cno_tv_scrape() {
        let res = scrape("1396", MediaKind::Tv, 1, 1, Some("Breaking Bad"), Some(2008)).await.unwrap();
        assert!(!res.sources.is_empty());
        assert!(res.sources[0].is_direct());
    }
}
