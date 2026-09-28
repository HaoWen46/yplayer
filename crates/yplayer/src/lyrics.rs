use crate::http::HttpGet;
use regex_lite::Regex;
use std::sync::LazyLock;
use std::time::Duration;

/// Parse LRC text into `(seconds, line)` pairs, sorted by time. Metadata tags
/// like `[ar:...]` are skipped; a line may carry several timestamps.
pub fn parse_lrc(lrc: &str) -> Vec<(f64, String)> {
    let mut out: Vec<(f64, String)> = Vec::new();
    for line in lrc.lines() {
        let mut rest = line;
        let mut times: Vec<f64> = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(close) = stripped.find(']') else {
                break;
            };
            if let Some(t) = parse_lrc_time(&stripped[..close]) {
                times.push(t);
            }
            rest = &stripped[close + 1..];
        }
        let text = rest.trim();
        for t in times {
            out.push((t, text.to_string()));
        }
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Parse an LRC timestamp tag like `mm:ss.xx`; returns None for metadata tags.
fn parse_lrc_time(tag: &str) -> Option<f64> {
    let (m, s) = tag.split_once(':')?;
    let mins: f64 = m.trim().parse().ok()?;
    let secs: f64 = s.trim().parse().ok()?;
    Some(mins * 60.0 + secs)
}

pub const USER_AGENT: &str = "yplayer (https://github.com/HaoWen46/yplayer)";
const LRCLIB: &str = "https://lrclib.net";
const TIMEOUT: Duration = Duration::from_secs(10);

/// Reduce a decorated YouTube title to the song name for lyrics matching
/// (port of `yplayer/core.py::_clean_track_title`).
///
/// jpop uploads are typically `Artist『Song』MV (romaji ...)`. Prefer the text
/// inside Japanese quote brackets; otherwise strip bracketed decorations and
/// common tags (MV, Official Video, feat. ...).
pub fn clean_track_title(title: &str) -> String {
    static QUOTED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[『「【]([^』」】]+)[』」】]").expect("valid regex"));
    static BRACKETS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[\(\[（【][^\)\]）】]*[\)\]）】]").expect("valid regex"));
    static TAGS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:MV|M/V|Music Video|Official.*|Lyric.*|feat\..*)\b")
            .expect("valid regex")
    });
    if let Some(c) = QUOTED.captures(title) {
        return c[1].trim().to_string();
    }
    let t = BRACKETS.replace_all(title, "");
    let t = TAGS.replace_all(&t, "");
    t.trim().to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub enum LyricsOutcome {
    Found { synced: bool, body: String },
    Missing,
    Error(String),
}

/// Look up lyrics on LRCLIB: exact `/api/get`, then `/api/search` candidates.
/// A 429 stops with `Error`; a transport error or 5xx only skips that call,
/// and makes the result `Error` instead of `Missing` when nothing was found
/// (so a transient failure is never cached as a miss).
pub fn fetch(
    http: &dyn HttpGet,
    title: &str,
    artist: Option<&str>,
    duration: Option<i64>,
) -> LyricsOutcome {
    let cleaned = clean_track_title(title);
    let mut plain: Option<String> = None;
    let mut failed: Option<String> = None;

    if let (Some(artist), Some(d)) = (artist, duration) {
        let d = d.to_string();
        let query = encode_query(&[
            ("track_name", &cleaned),
            ("artist_name", artist),
            ("duration", &d),
        ]);
        let body = match lrclib_get(http, &format!("{LRCLIB}/api/get?{query}")) {
            Ok(Some(body)) => body,
            Ok(None) => serde_json::Value::Null,
            Err(CallError::RateLimited(e)) => return LyricsOutcome::Error(e),
            Err(CallError::Failed(e)) => {
                failed = Some(e);
                serde_json::Value::Null
            }
        };
        if let Some(synced) = non_empty(&body, "syncedLyrics") {
            return LyricsOutcome::Found {
                synced: true,
                body: synced,
            };
        }
        plain = non_empty(&body, "plainLyrics");
    }

    let artist = artist.unwrap_or("");
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for cand in [
        (cleaned.as_str(), artist),
        (cleaned.as_str(), ""),
        (title, artist),
    ] {
        if cand.0.is_empty() || seen.contains(&cand) {
            continue;
        }
        seen.push(cand);
        let query = encode_query(&[("track_name", cand.0), ("artist_name", cand.1)]);
        let hits = match lrclib_get(http, &format!("{LRCLIB}/api/search?{query}")) {
            Ok(Some(serde_json::Value::Array(hits))) => hits,
            Ok(_) => continue,
            Err(CallError::RateLimited(e)) => return LyricsOutcome::Error(e),
            Err(CallError::Failed(e)) => {
                failed = Some(e);
                continue;
            }
        };
        let mut best: Option<(f64, String)> = None;
        for hit in &hits {
            if plain.is_none() {
                plain = non_empty(hit, "plainLyrics");
            }
            let Some(synced) = non_empty(hit, "syncedLyrics") else {
                continue;
            };
            let rd = hit.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let score = (duration.map(|d| d as f64).unwrap_or(rd) - rd).abs();
            if best.as_ref().is_none_or(|(s, _)| score < *s) {
                best = Some((score, synced));
            }
        }
        if let Some((_, synced)) = best {
            return LyricsOutcome::Found {
                synced: true,
                body: synced,
            };
        }
    }

    match (plain, failed) {
        (Some(body), _) => LyricsOutcome::Found {
            synced: false,
            body,
        },
        (None, Some(e)) => LyricsOutcome::Error(e),
        (None, None) => LyricsOutcome::Missing,
    }
}

enum CallError {
    /// HTTP 429: stop the whole lookup.
    RateLimited(String),
    /// Transport error or 5xx: skip this call.
    Failed(String),
}

/// One LRCLIB call. `Err` on transport error, 429 or 5xx; `Ok(None)` on any
/// other non-200 status or an unparsable body.
fn lrclib_get(http: &dyn HttpGet, url: &str) -> Result<Option<serde_json::Value>, CallError> {
    let resp = http
        .get(url, TIMEOUT)
        .map_err(|e| CallError::Failed(format!("LRCLIB request failed: {e:#}")))?;
    if resp.status == 429 {
        return Err(CallError::RateLimited(
            "LRCLIB returned HTTP 429".to_string(),
        ));
    }
    if resp.status >= 500 {
        return Err(CallError::Failed(format!(
            "LRCLIB returned HTTP {}",
            resp.status
        )));
    }
    if resp.status != 200 {
        return Ok(None);
    }
    Ok(serde_json::from_slice(&resp.body).ok())
}

fn non_empty(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `application/x-www-form-urlencoded` query string, like Python's `urlencode`.
fn encode_query(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(k);
        out.push('=');
        for b in v.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                    out.push(b as char)
                }
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;
    use std::sync::Mutex;

    type Responder = Box<dyn Fn(&str) -> anyhow::Result<HttpResponse> + Send + Sync>;

    struct FakeHttp {
        calls: Mutex<Vec<String>>,
        respond: Responder,
    }

    impl FakeHttp {
        fn new(
            respond: impl Fn(&str) -> anyhow::Result<HttpResponse> + Send + Sync + 'static,
        ) -> Self {
            FakeHttp {
                calls: Mutex::new(Vec::new()),
                respond: Box::new(respond),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl HttpGet for FakeHttp {
        fn get(&self, url: &str, _timeout: Duration) -> anyhow::Result<HttpResponse> {
            self.calls.lock().unwrap().push(url.to_string());
            (self.respond)(url)
        }

        fn head_no_redirect(&self, _url: &str, _timeout: Duration) -> anyhow::Result<HttpResponse> {
            unreachable!("lyrics never sends HEAD")
        }
    }

    fn resp(status: u16, body: &str) -> anyhow::Result<HttpResponse> {
        Ok(HttpResponse {
            status,
            body: body.as_bytes().to_vec(),
            location: None,
        })
    }

    fn is_get(url: &str) -> bool {
        url.starts_with("https://lrclib.net/api/get?")
    }

    fn is_search(url: &str) -> bool {
        url.starts_with("https://lrclib.net/api/search?")
    }

    const MIRABO: &str = "ZUTOMAYO「ミラボ」Official Video";

    #[test]
    fn clean_track_title_matches_python() {
        // Expected values produced by yplayer/core.py::_clean_track_title.
        let cases = [
            ("ずっと真夜中でいいのに。『秒針を噛む』MV", "秒針を噛む"),
            ("ZUTOMAYO「ミラボ」Official Video", "ミラボ"),
            ("Song Name (Official Music Video)", "Song Name"),
            ("Artist - Title [Lyric Video]", "Artist - Title"),
            ("Title feat. Someone", "Title"),
            ("【MV】Title", "MV"),
            ("Title", "Title"),
        ];
        for (input, expected) in cases {
            assert_eq!(clean_track_title(input), expected, "input: {input}");
        }
    }

    #[test]
    fn get_hit_returns_synced_without_search() {
        let http = FakeHttp::new(|url| {
            assert!(is_get(url), "unexpected call {url}");
            resp(200, r#"{"syncedLyrics":"[00:01.00]hi","plainLyrics":"hi"}"#)
        });
        let out = fetch(&http, MIRABO, Some("ZUTOMAYO"), Some(200));
        assert_eq!(
            out,
            LyricsOutcome::Found {
                synced: true,
                body: "[00:01.00]hi".to_string()
            }
        );
        assert_eq!(http.calls().len(), 1);
    }

    #[test]
    fn get_miss_then_search_picks_closest_duration() {
        let http = FakeHttp::new(|url| {
            if is_get(url) {
                return resp(404, r#"{"code":404}"#);
            }
            resp(
                200,
                r#"[{"duration":180.0,"syncedLyrics":"far"},
                    {"duration":199.0,"syncedLyrics":"near"},
                    {"duration":200.0,"plainLyrics":"plain only"}]"#,
            )
        });
        let out = fetch(&http, MIRABO, Some("ZUTOMAYO"), Some(200));
        assert_eq!(
            out,
            LyricsOutcome::Found {
                synced: true,
                body: "near".to_string()
            }
        );
        let calls = http.calls();
        assert_eq!(calls.len(), 2);
        assert!(is_get(&calls[0]));
        assert!(is_search(&calls[1]));
    }

    #[test]
    fn duplicate_candidates_requested_once() {
        let http = FakeHttp::new(|_| resp(200, "[]"));
        assert_eq!(fetch(&http, "Title", None, None), LyricsOutcome::Missing);
        assert_eq!(http.calls().len(), 1);

        let http = FakeHttp::new(|_| resp(200, "[]"));
        assert_eq!(
            fetch(&http, "Title", Some("A"), None),
            LyricsOutcome::Missing
        );
        let calls = http.calls();
        assert_eq!(
            calls,
            vec![
                "https://lrclib.net/api/search?track_name=Title&artist_name=A".to_string(),
                "https://lrclib.net/api/search?track_name=Title&artist_name=".to_string(),
            ]
        );
    }

    #[test]
    fn plain_only_returns_unsynced() {
        let http = FakeHttp::new(|_| resp(200, r#"[{"duration":200,"plainLyrics":"words"}]"#));
        let out = fetch(&http, "Title", None, Some(200));
        assert_eq!(
            out,
            LyricsOutcome::Found {
                synced: false,
                body: "words".to_string()
            }
        );
    }

    #[test]
    fn rate_limit_is_error_and_stops() {
        let http = FakeHttp::new(|_| resp(429, ""));
        let out = fetch(&http, MIRABO, Some("ZUTOMAYO"), Some(200));
        assert!(matches!(out, LyricsOutcome::Error(_)), "{out:?}");
        assert_eq!(http.calls().len(), 1);
    }

    #[test]
    fn transient_failures_skip_calls_and_are_never_a_miss() {
        // Every call fails: all candidates are tried, and the result is an
        // Error (not cached), never Missing.
        let http = FakeHttp::new(|_| resp(503, ""));
        let out = fetch(&http, MIRABO, None, None);
        assert!(matches!(out, LyricsOutcome::Error(_)), "{out:?}");
        assert!(http.calls().len() > 1);

        let http = FakeHttp::new(|_| Err(anyhow::anyhow!("connection refused")));
        let out = fetch(&http, MIRABO, None, None);
        assert!(matches!(out, LyricsOutcome::Error(_)), "{out:?}");
    }

    #[test]
    fn get_timeout_then_search_hit_is_found() {
        let http = FakeHttp::new(|url| {
            if is_get(url) {
                return Err(anyhow::anyhow!("curl: (28) Operation timed out"));
            }
            resp(
                200,
                r#"[{"duration": 200, "syncedLyrics": "[00:01.00] la"}]"#,
            )
        });
        let out = fetch(&http, MIRABO, Some("ZUTOMAYO"), Some(200));
        assert_eq!(
            out,
            LyricsOutcome::Found {
                synced: true,
                body: "[00:01.00] la".to_string()
            }
        );
    }

    #[test]
    fn all_empty_is_missing() {
        let http = FakeHttp::new(|url| {
            if is_get(url) {
                return resp(404, r#"{"code":404}"#);
            }
            resp(200, "[]")
        });
        assert_eq!(
            fetch(&http, MIRABO, Some("ZUTOMAYO"), Some(200)),
            LyricsOutcome::Missing
        );
        let calls = http.calls();
        assert_eq!(calls.len(), 4);
        assert!(is_get(&calls[0]));
        assert!(calls[1..].iter().all(|u| is_search(u)));
    }

    #[test]
    fn missing_artist_or_duration_skips_get() {
        let http = FakeHttp::new(|_| resp(200, "[]"));
        fetch(&http, MIRABO, None, Some(200));
        fetch(&http, MIRABO, Some("ZUTOMAYO"), None);
        let calls = http.calls();
        assert!(!calls.is_empty());
        assert!(calls.iter().all(|u| is_search(u)), "{calls:?}");
    }

    #[test]
    fn query_params_are_percent_encoded() {
        let http = FakeHttp::new(|_| resp(200, r#"{"syncedLyrics":"[00:01.00]x"}"#));
        fetch(
            &http,
            "ずっと真夜中でいいのに。『秒針を噛む』MV",
            Some("ずっと真夜中でいいのに。"),
            Some(245),
        );
        assert_eq!(
            http.calls(),
            vec![
                "https://lrclib.net/api/get?track_name=%E7%A7%92%E9%87%9D%E3%82%92%E5%99%9B%E3%82%80&artist_name=%E3%81%9A%E3%81%A3%E3%81%A8%E7%9C%9F%E5%A4%9C%E4%B8%AD%E3%81%A7%E3%81%84%E3%81%84%E3%81%AE%E3%81%AB%E3%80%82&duration=245".to_string()
            ]
        );
    }

    #[test]
    fn parse_lrc_sorts_and_skips_metadata() {
        let lrc = "[ar:Zutomayo]\n[00:12.50]first\n[00:15.00][01:00.00]repeat\n[00:03.00]early\nno timestamp\n";
        let parsed = super::parse_lrc(lrc);
        assert_eq!(
            parsed,
            vec![
                (3.0, "early".to_string()),
                (12.5, "first".to_string()),
                (15.0, "repeat".to_string()),
                (60.0, "repeat".to_string()),
            ]
        );
    }
}
