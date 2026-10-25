//! Which requests need a Streams account, and the check itself.
//!
//! Streams needs an account to watch (the site's Terms and Help Centre), and the web app sends the member's Supabase access token
//! with every request that resolves a stream. Before this check the resolver answered anyone who called it directly, so the
//! account requirement held only inside the app. A token is checked by asking Supabase who it belongs to (`GET /auth/v1/user`),
//! which also refuses a token whose session has been signed out; an accepted token is remembered for a few minutes so playback
//! does not wait on Supabase for every request.
//!
//! Settings (all optional): `SUPABASE_URL` and `SUPABASE_PUBLISHABLE_KEY` (default: the project the site uses; both are public
//! values the web app also carries), `REQUIRE_ACCOUNT=false` to open the routes again.

use axum::http::HeaderMap;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_SUPABASE_URL: &str = "https://emvyjhleyhimxxdqybsn.supabase.co";
const DEFAULT_PUBLISHABLE_KEY: &str = "sb_publishable_m286TDvNziieAgk--uDLlQ_MJQHgxNV";

/// How long an accepted token is trusted without asking again. A session signed out elsewhere keeps working this long at most.
const TRUST_FOR: Duration = Duration::from_secs(5 * 60);
/// Remembered tokens at most; past this the oldest half is dropped.
const MAX_REMEMBERED: usize = 5_000;

/// What the check found.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// a signed in member (or the check is switched off)
    Member,
    /// no token, or one Supabase does not accept
    Refused,
    /// Supabase could not be asked; the request is refused, but the app can say "try again" rather than "sign in"
    Unavailable,
}

static ACCEPTED: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()
        .expect("account check client")
});

fn setting(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

/// Whether the account check is on. Off until the web app sending the token is live everywhere (step 1 of 3, 2026-10-25):
/// `REQUIRE_ACCOUNT=true` turns it on; the next release makes on the default.
pub fn required() -> bool {
    matches!(std::env::var("REQUIRE_ACCOUNT").unwrap_or_default().trim().to_ascii_lowercase().as_str(), "true" | "1" | "on" | "yes")
}

/// The bearer token of a request, if it carries one.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?.trim();
    let token = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer "))?.trim();
    (token.len() >= 20 && token.len() <= 4096 && !token.contains(char::is_whitespace)).then_some(token)
}

/// Whether the request comes from a signed in member.
pub async fn check(headers: &HeaderMap) -> Verdict {
    if !required() {
        return Verdict::Member;
    }
    let Some(token) = bearer(headers) else { return Verdict::Refused };
    if let Some(at) = ACCEPTED.lock().ok().and_then(|m| m.get(token).copied()) {
        if at.elapsed() < TRUST_FOR {
            return Verdict::Member;
        }
    }
    let url = format!("{}/auth/v1/user", setting("SUPABASE_URL", DEFAULT_SUPABASE_URL).trim_end_matches('/'));
    let answer = CLIENT
        .get(&url)
        .header("apikey", setting("SUPABASE_PUBLISHABLE_KEY", DEFAULT_PUBLISHABLE_KEY))
        .bearer_auth(token)
        .send()
        .await;
    match answer {
        Ok(r) if r.status().is_success() => {
            if let Ok(mut m) = ACCEPTED.lock() {
                if m.len() >= MAX_REMEMBERED {
                    let mut ages: Vec<(String, Instant)> = m.drain().collect();
                    ages.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
                    m.extend(ages.into_iter().take(MAX_REMEMBERED / 2));
                }
                m.insert(token.to_string(), Instant::now());
            }
            Verdict::Member
        }
        Ok(r) if r.status().is_client_error() => Verdict::Refused,
        _ => Verdict::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn with(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_str(value).unwrap());
        h
    }

    #[test]
    fn reads_only_a_plausible_bearer_token() {
        assert_eq!(bearer(&with("Bearer eyJhbGciOiJFUzI1NiJ9.payload.sig")), Some("eyJhbGciOiJFUzI1NiJ9.payload.sig"));
        assert_eq!(bearer(&with("Bearer short")), None);
        assert_eq!(bearer(&with("Basic eyJhbGciOiJFUzI1NiJ9.payload.sig")), None);
        assert_eq!(bearer(&HeaderMap::new()), None);
    }

    #[tokio::test]
    async fn no_token_is_refused_without_asking_supabase() {
        if required() {
            assert_eq!(check(&HeaderMap::new()).await, Verdict::Refused);
        }
    }
}
