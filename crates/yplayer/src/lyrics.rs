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

#[cfg(test)]
mod tests {
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
