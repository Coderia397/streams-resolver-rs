use crate::http::{get_text_with, GIGA, PROXY};
use crate::models::{MediaKind, ProviderResult, Source};
use crate::utils::{matches_year_tolerance, sort_sources_by_quality};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::time::Duration;

pub const ID: &str = "arc018";
pub const NAME: &str = "Arc018 ⚡";
pub const BASE: &str = "https://arc018.stream";

const HEADERS: &[(&str, &str)] = &[
    ("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
    ("Accept-Language", "en-US,en;q=0.9"),
    ("Referer", "https://arc018.stream/"),
];

static SEARCH_MOVIE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"href=["'](?:https?://arc018\.stream)?/watch-movie/([^/"']+)/?["']"#)
        .expect("search movie regex")
});

static SEARCH_TV_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"href=["'](?:https?://arc018\.stream)?/watch-series/([^/"']+)/?["']"#)
        .expect("search tv regex")
});

static TOKEN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"data-token=["']([^"']+)["']"#).expect("data-token regex")
});

static VIDMOLY_FILE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"sources:\s*\[\{\s*file:\s*['"](https?://[^'"]+master\.m3u8[^'"]*)['"]"#)
        .expect("vidmoly file regex")
});

static SLUG_YEAR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"-(19\d{2}|20\d{2})-"#).expect("slug year regex")
});

/// Extracts candidate watch URL or slug from search HTML, matching year if provided.
pub fn parse_search_slug(html: &str, kind: MediaKind, target_year: Option<u32>) -> Option<String> {
    let re = if kind.is_movie() {
        &*SEARCH_MOVIE_RE
    } else {
        &*SEARCH_TV_RE
    };

    let mut first_candidate: Option<String> = None;

    for cap in re.captures_iter(html) {
        let slug = cap.get(1)?.as_str().to_string();
        if first_candidate.is_none() {
            first_candidate = Some(slug.clone());
        }

        if let Some(target) = target_year {
            if let Some(slug_yr) = SLUG_YEAR_RE.captures(&slug).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse::<u32>().ok()) {
                if matches_year_tolerance(Some(target), Some(slug_yr), 1) {
                    return Some(slug);
                }
            }
        } else {
            return Some(slug);
        }
    }

    first_candidate
}

/// Extracts the player session token from HTML markup.
pub fn extract_player_token(html: &str) -> Option<String> {
    TOKEN_RE.captures(html).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
}

/// Extracts direct master manifest URL from Vidmoly embed markup.
pub fn extract_vidmoly_master(html: &str) -> Option<String> {
    VIDMOLY_FILE_RE.captures(html).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
}

/// Parses an HLS master playlist into discrete Source instances labeled by resolution.
pub fn parse_master_m3u8(body: &str, master_url: &str) -> Vec<Source> {
    let mut sources = Vec::new();
    let mut pending_quality: Option<String> = None;

    // Master playlist kept first as adaptive "auto"
    if body.trim_start().starts_with("#EXTM3U") {
        let mut auto_master = Source::direct_m3u8(master_url, "auto").tagged(NAME, ID);
        auto_master = auto_master.not_embed();
        sources.push(auto_master);
    }

    let mut variant_sources = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with("#EXT-X-STREAM-INF:") {
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
                    match reqwest::Url::parse(master_url).and_then(|base| base.join(trimmed)) {
                        Ok(resolved) => resolved.to_string(),
                        Err(_) => format!("{BASE}/{trimmed}"),
                    }
                };

                let mut s = Source::direct_m3u8(stream_url, quality).tagged(NAME, ID);
                s = s.not_embed();
                variant_sources.push(s);
            }
        }
    }

    sort_sources_by_quality(&mut variant_sources);
    sources.extend(variant_sources);
    sources
}

pub async fn scrape(
    _tmdb_id: &str,
    kind: MediaKind,
    season: u32,
    episode: u32,
    title: Option<&str>,
    year: Option<u32>,
) -> Option<ProviderResult> {
    let title = title?.trim();
    if title.is_empty() {
        return None;
    }

    // 1. Search for title
    let encoded_query = urlencoding::encode(title);
    let search_url = format!("{BASE}/search?keyword={encoded_query}");

    let search_html = match get_text_with(&GIGA, &search_url, Duration::from_secs(6), HEADERS).await {
        Some(h) => h,
        None => get_text_with(&PROXY, &search_url, Duration::from_secs(8), HEADERS).await?,
    };

    let slug = parse_search_slug(&search_html, kind, year)?;

    // 2. Fetch watch page
    let watch_url = if kind.is_movie() {
        format!("{BASE}/watch-movie/{slug}/")
    } else {
        format!("{BASE}/episode/{slug}/s{season:02}-e{episode:02}/")
    };

    let page_html = match get_text_with(&GIGA, &watch_url, Duration::from_secs(6), HEADERS).await {
        Some(h) => h,
        None => get_text_with(&PROXY, &watch_url, Duration::from_secs(8), HEADERS).await?,
    };

    let token = extract_player_token(&page_html)?;

    // 3. Request server links from ajax endpoint
    let ajax_url = format!("{BASE}/ajax/ajax.php");
    let param_name = if kind.is_movie() { "players" } else { "players_show" };

    let client = &*GIGA;
    let ajax_resp = client
        .post(&ajax_url)
        .header("Referer", &watch_url)
        .header("User-Agent", crate::http::random_ua())
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("{param_name}={}", urlencoding::encode(&token)))
        .send()
        .await
        .ok()?;

    let servers: Vec<Value> = ajax_resp.json().await.ok()?;
    if servers.is_empty() {
        return None;
    }

    let mut sources = Vec::new();

    // 4. Resolve servers: inspect Vidmoly for direct manifests & add clean embeds
    for server in &servers {
        let Some(link) = server.get("link").and_then(Value::as_str) else {
            continue;
        };
        let name = server.get("name").and_then(Value::as_str).unwrap_or("Server");

        // Vidmoly direct stream extraction
        if link.contains("kaembed.net") || name.to_lowercase().contains("vidmoly") {
            let vidmoly_headers = &[
                ("Referer", "https://arc018.stream/"),
                ("Accept", "*/*"),
            ];

            if let Some(v_html) = get_text_with(&GIGA, link, Duration::from_secs(6), vidmoly_headers).await {
                if let Some(master_url) = extract_vidmoly_master(&v_html) {
                    if let Some(master_body) = get_text_with(&GIGA, &master_url, Duration::from_secs(6), vidmoly_headers).await {
                        let parsed = parse_master_m3u8(&master_body, &master_url);
                        if !parsed.is_empty() {
                            sources.extend(parsed);
                        }
                    }
                }
            }
        }

        // Register each playable server link as an embed fallback
        let embed_source = Source::embed(link, "auto")
            .tagged(NAME, ID)
            .with_referer(BASE);
        sources.push(embed_source);
    }

    if sources.is_empty() {
        None
    } else {
        sort_sources_by_quality(&mut sources);
        ProviderResult::some_if_any(NAME, ID, sources)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOCK_SEARCH: &str = r#"
        <a class="block" href="https://arc018.stream/watch-movie/inception-2010-watch-online/">
          <article>
            <div class="meta"><span>2010</span></div>
          </article>
        </a>
        <a class="block" href="https://arc018.stream/watch-movie/inception-the-cobol-job-2010-watch-online/">
          <article></article>
        </a>
    "#;

    const MOCK_TV_SEARCH: &str = r#"
        <a class="block" href="https://arc018.stream/watch-series/breaking-bad-2008-watch-online/">
          <article></article>
        </a>
    "#;

    const MOCK_PAGE: &str = r#"
        <section class="player" data-token="test_token_12345">
          <div id="player"></div>
        </section>
    "#;

    const MOCK_VIDMOLY: &str = r#"
        var file_code = 'rqxbkte4zx9p';
        sources: [{ file: 'https://prx.vmpx.online/hls2/master.m3u8?t=test' }],
    "#;

    const MOCK_MASTER: &str = r#"#EXTM3U
#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=4041473,RESOLUTION=1920x1080
https://prx.vmpx.online/hls2/1080p/index.m3u8
#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH=553625,RESOLUTION=852x480
https://prx.vmpx.online/hls2/480p/index.m3u8
"#;

    #[test]
    fn parses_movie_search_slug() {
        let slug = parse_search_slug(MOCK_SEARCH, MediaKind::Movie, Some(2010)).unwrap();
        assert_eq!(slug, "inception-2010-watch-online");
    }

    #[test]
    fn parses_tv_search_slug() {
        let slug = parse_search_slug(MOCK_TV_SEARCH, MediaKind::Tv, Some(2008)).unwrap();
        assert_eq!(slug, "breaking-bad-2008-watch-online");
    }

    #[test]
    fn extracts_token_successfully() {
        let token = extract_player_token(MOCK_PAGE).unwrap();
        assert_eq!(token, "test_token_12345");
    }

    #[test]
    fn extracts_vidmoly_master_file() {
        let master = extract_vidmoly_master(MOCK_VIDMOLY).unwrap();
        assert_eq!(master, "https://prx.vmpx.online/hls2/master.m3u8?t=test");
    }

    #[test]
    fn parses_master_m3u8_streams() {
        let sources = parse_master_m3u8(MOCK_MASTER, "https://prx.vmpx.online/hls2/master.m3u8");
        assert_eq!(sources.len(), 3);
        assert_eq!(sources[0].quality, "auto");
        assert_eq!(sources[0].url, "https://prx.vmpx.online/hls2/master.m3u8");
        assert_eq!(sources[1].quality, "1080p");
        assert_eq!(sources[2].quality, "480p");
    }

    #[tokio::test]
    #[ignore]
    async fn live_arc018_movie_scrape() {
        let res = scrape("27205", MediaKind::Movie, 1, 1, Some("Inception"), Some(2010)).await.unwrap();
        assert!(!res.sources.is_empty());
        assert!(res.sources.iter().any(|s| s.is_direct()));
    }

    #[tokio::test]
    #[ignore]
    async fn live_arc018_tv_scrape() {
        let res = scrape("1396", MediaKind::Tv, 1, 1, Some("Breaking Bad"), Some(2008)).await.unwrap();
        assert!(!res.sources.is_empty());
        assert!(res.sources.iter().any(|s| s.is_direct()));
    }
}
