#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    NotYouTube,
    PlaylistOnly,
    NoVideoId,
}

/// Extract the 11-char video id from a YouTube URL (scheme optional) or a
/// bare id. `&list=` is ignored when a `v` is present.
pub fn parse_video_id(input: &str) -> Result<String, UrlError> {
    let input = input.trim();
    if is_video_id(input) {
        return Ok(input.to_string());
    }

    let lower = input.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &input[8..]
    } else if lower.starts_with("http://") {
        &input[7..]
    } else {
        input
    };

    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest[..host_end].to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or("");
    let host = ["www.", "m.", "music."]
        .iter()
        .find_map(|p| host.strip_prefix(p))
        .unwrap_or(host);

    let tail = &rest[host_end..];
    let tail = tail.split('#').next().unwrap_or("");
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    let mut segments = path.split('/').filter(|s| !s.is_empty());

    match host {
        "youtu.be" => video_id(segments.next()),
        "youtube.com" => match segments.next() {
            Some("watch") => match query_param(query, "v") {
                Some(v) => video_id(Some(v)),
                None if query_param(query, "list").is_some() => Err(UrlError::PlaylistOnly),
                None => Err(UrlError::NoVideoId),
            },
            Some("playlist") if query_param(query, "list").is_some() => Err(UrlError::PlaylistOnly),
            Some("shorts" | "live" | "embed") => video_id(segments.next()),
            _ => Err(UrlError::NoVideoId),
        },
        _ => Err(UrlError::NotYouTube),
    }
}

fn video_id(candidate: Option<&str>) -> Result<String, UrlError> {
    match candidate {
        Some(id) if is_video_id(id) => Ok(id.to_string()),
        _ => Err(UrlError::NoVideoId),
    }
}

fn is_video_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|pair| pair.split_once('=').filter(|(k, _)| *k == key))
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "dQw4w9WgXcQ";

    #[test]
    fn accepts_all_supported_forms() {
        let inputs = [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "http://youtube.com/watch?v=dQw4w9WgXcQ",
            "youtube.com/watch?v=dQw4w9WgXcQ",
            "www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLabc&index=2",
            "https://www.youtube.com/watch?list=PLabc&v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "youtu.be/dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=x",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://youtube.com/shorts/dQw4w9WgXcQ?feature=share",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "dQw4w9WgXcQ",
            "  https://youtu.be/dQw4w9WgXcQ  ",
        ];
        for input in inputs {
            assert_eq!(parse_video_id(input), Ok(ID.to_string()), "{input}");
        }
        assert_eq!(parse_video_id("a-_B9cD8eF7"), Ok("a-_B9cD8eF7".to_string()));
    }

    #[test]
    fn rejects_playlist_only() {
        for input in [
            "https://www.youtube.com/playlist?list=PLabc",
            "youtube.com/playlist?list=PLabc",
            "https://www.youtube.com/watch?list=PLabc",
            "https://music.youtube.com/watch?list=PLabc&index=1",
        ] {
            assert_eq!(
                parse_video_id(input),
                Err(UrlError::PlaylistOnly),
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_other_hosts() {
        for input in [
            "https://vimeo.com/123",
            "vimeo.com/123",
            "https://notyoutube.com/watch?v=dQw4w9WgXcQ",
        ] {
            assert_eq!(parse_video_id(input), Err(UrlError::NotYouTube), "{input}");
        }
    }

    #[test]
    fn rejects_bad_video_id() {
        for input in [
            "https://www.youtube.com/watch?v=short",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQx",
            "https://youtu.be/short",
        ] {
            assert_eq!(parse_video_id(input), Err(UrlError::NoVideoId), "{input}");
        }
    }
}
