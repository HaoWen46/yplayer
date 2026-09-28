use anyhow::{Context, bail};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Status, body and (for `head_no_redirect`) the `Location` header of one request.
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub location: Option<String>,
}

/// Blocking HTTP GET/HEAD; callers run it on `spawn_blocking`. A non-2xx
/// status is `Ok`; only process/transport failure is `Err`.
pub trait HttpGet: Send + Sync {
    fn get(&self, url: &str, timeout: Duration) -> anyhow::Result<HttpResponse>;
    fn head_no_redirect(&self, url: &str, timeout: Duration) -> anyhow::Result<HttpResponse>;
}

/// `HttpGet` backed by a `curl` subprocess (no TLS stack in the binary).
pub struct CurlHttp {
    pub user_agent: String,
}

impl CurlHttp {
    /// Run curl once; the status code is appended to stdout via `-w`.
    /// `head` sends `-I` (no `-L`) and extracts the `Location` header.
    fn run(&self, url: &str, timeout: Duration, head: bool) -> anyhow::Result<HttpResponse> {
        let curl = if Path::new("/usr/bin/curl").exists() {
            "/usr/bin/curl"
        } else {
            "curl"
        };
        let mut cmd = Command::new(curl);
        cmd.args([
            "-q",
            "--proto",
            "=https",
            "--max-filesize",
            "5000000",
            "-sS",
            "--max-time",
        ])
        .arg(timeout.as_secs().to_string())
        .arg("-A")
        .arg(&self.user_agent)
        .args(["-w", "%{http_code}"]);
        if head {
            cmd.arg("-I");
        }
        let out = cmd
            .arg(url)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("failed to run {curl}"))?;
        if !out.status.success() {
            bail!(
                "curl {url} failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let Some(split) = out.stdout.len().checked_sub(3) else {
            bail!("curl {url}: missing status code");
        };
        let (body, code) = out.stdout.split_at(split);
        let status: u16 = std::str::from_utf8(code)?
            .parse()
            .with_context(|| format!("curl {url}: bad status code"))?;
        if !head {
            return Ok(HttpResponse {
                status,
                body: body.to_vec(),
                location: None,
            });
        }
        let location = String::from_utf8_lossy(body).lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("location")
                .then(|| value.trim().to_string())
        });
        Ok(HttpResponse {
            status,
            body: Vec::new(),
            location,
        })
    }
}

impl HttpGet for CurlHttp {
    fn get(&self, url: &str, timeout: Duration) -> anyhow::Result<HttpResponse> {
        self.run(url, timeout, false)
    }

    fn head_no_redirect(&self, url: &str, timeout: Duration) -> anyhow::Result<HttpResponse> {
        self.run(url, timeout, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn curl_http_live_lrclib_search() {
        let http = CurlHttp {
            user_agent: crate::lyrics::USER_AGENT.to_string(),
        };
        let resp = http
            .get(
                "https://lrclib.net/api/search?track_name=ham&artist_name=zutomayo",
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(resp.status, 200);
        let hits: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
        assert!(hits.is_array());
    }
}
