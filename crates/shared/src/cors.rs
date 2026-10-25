//! Origin allowlist — port of `corsFor` in `local-resolver/server.mjs`.
//!
//! Only our own frontends may call this from a browser. This does not stop
//! scripted abuse (curl ignores CORS entirely — that's what the rate limiter
//! is for), but it does stop another website embedding this resolver and
//! using it as free infrastructure paid for in someone's mobile data.
//!
//! A disallowed origin still gets a valid CORS header, just one naming the
//! canonical site, so the browser refuses the response.

use axum::http::{header, HeaderMap, HeaderName, HeaderValue};
use once_cell::sync::Lazy;
use regex::Regex;

/// The site a refused browser is pointed at. Override with CANONICAL_ORIGIN.
const CANONICAL: &str = "https://streams.watch";

/// Built in origins: the current site, its help host, the previous site while it still redirects, and local dev servers.
const ALLOWED: &[&str] = &[
    "https://streams.watch",
    "https://www.streams.watch",
    "https://help.streams.watch",
    "https://pstream.watch",
    "https://www.pstream.watch",
    "https://help.pstream.watch",
    "http://localhost:5173",
    "http://localhost:5199",
    "http://localhost:4173",
    "http://localhost:8080",
];

/// More origins from the ALLOWED_ORIGINS environment variable, comma separated (for example a new domain or a staging site),
/// so adding a site does not need a new build.
static EXTRA: Lazy<Vec<String>> = Lazy::new(|| {
    std::env::var("ALLOWED_ORIGINS")
        .map(|v| v.split(',').map(|o| o.trim().trim_end_matches('/').to_string()).filter(|o| !o.is_empty()).collect())
        .unwrap_or_default()
});

static CANONICAL_OVERRIDE: Lazy<Option<String>> =
    Lazy::new(|| std::env::var("CANONICAL_ORIGIN").ok().map(|v| v.trim().trim_end_matches('/').to_string()).filter(|v| !v.is_empty()));

fn canonical() -> &'static str {
    CANONICAL_OVERRIDE.as_deref().unwrap_or(CANONICAL)
}

/// Cloudflare Pages preview deployments, e.g. `https://abc123.pages.dev`.
static PAGES_DEV: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^https://[a-z0-9-]+\.pages\.dev$").expect("pages.dev regex"));

fn is_allowed(origin: &str) -> bool {
    ALLOWED.contains(&origin) || EXTRA.iter().any(|o| o == origin) || PAGES_DEV.is_match(origin)
}

/// Build the CORS headers for a request, echoing the origin only if allowed.
///
/// `Vary: Origin` matters because the response differs per origin — without
/// it any cache in front of this would serve one origin's headers to another.
pub fn headers_for(req: &HeaderMap) -> [(HeaderName, HeaderValue); 5] {
    let origin = req.get(header::ORIGIN).and_then(|v| v.to_str().ok());

    // No Origin at all means a non-browser client; nothing to protect against
    // here, and echoing "*" keeps curl and the player's own fetches working.
    let allow = match origin {
        None => "*".to_string(),
        Some(o) if is_allowed(o) => o.to_string(),
        Some(_) => canonical().to_string(),
    };

    [
        (
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(&allow)
                .unwrap_or_else(|_| HeaderValue::from_static(CANONICAL)),
        ),
        (
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, OPTIONS"),
        ),
        (
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Content-Type, X-Deploy-Secret, Authorization, Range"),
        ),
        (header::VARY, HeaderValue::from_static("Origin")),
        // the app sends its account token on every stream request, which makes the browser ask first (a preflight); without this the
        // browser asks again before almost every request (Chrome keeps the answer 5 s by default, at most 2 h)
        (header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("7200")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin_header(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, HeaderValue::from_str(v).unwrap());
        h
    }

    fn allow_origin_of(h: &HeaderMap) -> String {
        headers_for(h)[0].1.to_str().unwrap().to_string()
    }

    #[test]
    fn echoes_an_allowed_origin() {
        assert_eq!(allow_origin_of(&origin_header("https://streams.watch")), "https://streams.watch");
        assert_eq!(allow_origin_of(&origin_header("https://help.streams.watch")), "https://help.streams.watch");
        assert_eq!(allow_origin_of(&origin_header("https://pstream.watch")), "https://pstream.watch");
        assert_eq!(allow_origin_of(&origin_header("http://localhost:5173")), "http://localhost:5173");
    }

    #[test]
    fn allows_pages_dev_previews() {
        assert_eq!(
            allow_origin_of(&origin_header("https://deploy-preview-7.pages.dev")),
            "https://deploy-preview-7.pages.dev"
        );
    }

    #[test]
    fn refuses_a_foreign_origin_by_naming_the_canonical_site() {
        assert_eq!(allow_origin_of(&origin_header("https://evil.example")), CANONICAL);
        // Must not be fooled by a lookalike host that merely ends in the string.
        assert_eq!(allow_origin_of(&origin_header("https://evil.pages.dev.attacker.com")), CANONICAL);
        assert_eq!(allow_origin_of(&origin_header("https://sub.evil.pages.dev")), CANONICAL);
    }

    #[test]
    fn wildcards_when_there_is_no_origin() {
        assert_eq!(allow_origin_of(&HeaderMap::new()), "*");
    }
}
