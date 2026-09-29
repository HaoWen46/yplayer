use serde::de::{self, Deserializer};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{Album, LoopMode, Track};

/// Maximum length of one request/response/event line, in bytes.
pub const MAX_LINE: usize = 1 << 20;
pub const PROTOCOL: u32 = 1;

/// Wire names of every `Command`, used to tell unknown commands from malformed ones.
const COMMANDS: [&str; 27] = [
    "hello",
    "subscribe",
    "library.get",
    "now",
    "add",
    "play",
    "pause",
    "resume",
    "toggle",
    "stop",
    "next",
    "prev",
    "seek",
    "volume",
    "loop",
    "queue.play_next",
    "album.create",
    "album.rename",
    "album.delete",
    "album.add",
    "album.remove",
    "album.reorder",
    "track.delete",
    "track.rename",
    "track.retry",
    "rescan",
    "lyrics",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub id: u64,
    #[serde(flatten)]
    pub cmd: Command,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum Command {
    #[serde(rename = "hello")]
    Hello { protocol: u32 },
    #[serde(rename = "subscribe")]
    Subscribe,
    #[serde(rename = "library.get")]
    LibraryGet,
    #[serde(rename = "now")]
    Now,
    #[serde(rename = "add")]
    Add {
        url: String,
        album: Option<AlbumRef>,
        #[serde(default = "default_true")]
        play: bool,
    },
    #[serde(rename = "play")]
    Play {
        track_id: String,
        context: ContextRef,
    },
    #[serde(rename = "pause")]
    Pause,
    #[serde(rename = "resume")]
    Resume,
    #[serde(rename = "toggle")]
    Toggle,
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "next")]
    Next,
    #[serde(rename = "prev")]
    Prev,
    #[serde(rename = "seek")]
    Seek { position: f64 },
    #[serde(rename = "volume")]
    Volume { value: f64 },
    #[serde(rename = "loop")]
    Loop { mode: LoopMode },
    #[serde(rename = "queue.play_next")]
    QueuePlayNext { track_id: String },
    #[serde(rename = "album.create")]
    AlbumCreate { name: String },
    #[serde(rename = "album.rename")]
    AlbumRename { album_id: i64, name: String },
    #[serde(rename = "album.delete")]
    AlbumDelete { album_id: i64 },
    #[serde(rename = "album.add")]
    AlbumAdd { album_id: i64, track_id: String },
    #[serde(rename = "album.remove")]
    AlbumRemove { album_id: i64, track_id: String },
    #[serde(rename = "album.reorder")]
    AlbumReorder {
        album_id: i64,
        track_ids: Vec<String>,
    },
    #[serde(rename = "track.delete")]
    TrackDelete {
        track_id: String,
        #[serde(default = "default_true")]
        to_trash: bool,
    },
    #[serde(rename = "track.rename")]
    TrackRename { track_id: String, title: String },
    #[serde(rename = "track.retry")]
    TrackRetry { track_id: String },
    #[serde(rename = "rescan")]
    Rescan,
    #[serde(rename = "lyrics")]
    Lyrics { track_id: String },
}

/// `{"id": N}` or `{"name": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AlbumRef {
    Id { id: i64 },
    Name { name: String },
}

/// Wire shape `{"album_id": N}` or `{"library": true}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextRef {
    Album(i64),
    Library,
}

impl Serialize for ContextRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            ContextRef::Album(id) => map.serialize_entry("album_id", id)?,
            ContextRef::Library => map.serialize_entry("library", &true)?,
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ContextRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Album { album_id: i64 },
            Library { library: bool },
        }
        match Wire::deserialize(deserializer)? {
            Wire::Album { album_id } => Ok(ContextRef::Album(album_id)),
            Wire::Library { library: true } => Ok(ContextRef::Library),
            Wire::Library { library: false } => Err(de::Error::custom(
                "context must be {album_id} or {library: true}",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Response {
        Response {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, code: ErrorCode, message: impl Into<String>) -> Response {
        Response {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    UnknownCommand,
    ProtocolMismatch,
    NotFound,
    Conflict,
    InvalidUrl,
    UnsupportedUrl,
    DownloaderUnavailable,
    PlayerUnavailable,
    Internal,
}

/// Result of `add`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddResult {
    pub track_id: String,
    pub album_id: i64,
    pub was_new: bool,
    pub was_in_album: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayState {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    pub state: PlayState,
    pub track_id: Option<String>,
    pub context: Option<ContextRef>,
    pub position: f64,
    /// Wall-clock ms when `position` was sampled.
    pub at_ms: i64,
    pub duration: Option<f64>,
    pub volume: f64,
    #[serde(rename = "loop")]
    pub loop_mode: LoopMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadPhase {
    Fetching,
    Downloading,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum Event {
    #[serde(rename = "player")]
    Player(PlayerState),
    #[serde(rename = "track.upsert")]
    TrackUpsert { track: Track },
    #[serde(rename = "track.removed")]
    TrackRemoved { track_id: String },
    #[serde(rename = "album.upsert")]
    AlbumUpsert { album: Album },
    #[serde(rename = "album.removed")]
    AlbumRemoved { album_id: i64 },
    #[serde(rename = "download")]
    Download {
        track_id: String,
        phase: DownloadPhase,
        bytes: Option<u64>,
        total: Option<u64>,
        error: Option<String>,
    },
    #[serde(rename = "toast")]
    Toast { severity: Severity, message: String },
    #[serde(rename = "resync")]
    Resync,
}

/// Serialize `v` as one compact JSON line terminated by `\n`.
pub fn encode_line<T: Serialize>(v: &T) -> Vec<u8> {
    let mut line = serde_json::to_vec(v).expect("protocol types always serialize");
    line.push(b'\n');
    line
}

/// Parse one request line. On failure returns the error `Response` to send
/// back: `unknown_command` for an unrecognized `cmd`, else `bad_request`.
pub fn decode_request(line: &str) -> Result<RequestEnvelope, Response> {
    let value: Value = serde_json::from_str(line)
        .map_err(|e| Response::err(0, ErrorCode::BadRequest, e.to_string()))?;
    let id = value.get("id").and_then(Value::as_u64).unwrap_or(0);
    let Some(cmd) = value.get("cmd").and_then(Value::as_str) else {
        return Err(Response::err(id, ErrorCode::BadRequest, "missing cmd"));
    };
    if !COMMANDS.contains(&cmd) {
        return Err(Response::err(
            id,
            ErrorCode::UnknownCommand,
            format!("unknown command: {cmd}"),
        ));
    }
    serde_json::from_value(value)
        .map_err(|e| Response::err(id, ErrorCode::BadRequest, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Album, LoopMode, Track, TrackState};
    use serde_json::{Value, json};

    const ID: &str = "dQw4w9WgXcQ";

    #[test]
    fn every_command_round_trips_with_exact_wire_json() {
        let cases: Vec<(&str, Command)> = vec![
            (
                r#"{"id":1,"cmd":"hello","protocol":1}"#,
                Command::Hello { protocol: 1 },
            ),
            (r#"{"id":1,"cmd":"subscribe"}"#, Command::Subscribe),
            (r#"{"id":1,"cmd":"library.get"}"#, Command::LibraryGet),
            (r#"{"id":1,"cmd":"now"}"#, Command::Now),
            (
                r#"{"id":1,"cmd":"add","url":"https://youtu.be/dQw4w9WgXcQ","album":{"id":3},"play":false}"#,
                Command::Add {
                    url: "https://youtu.be/dQw4w9WgXcQ".into(),
                    album: Some(AlbumRef::Id { id: 3 }),
                    play: false,
                },
            ),
            (
                r#"{"id":1,"cmd":"play","track_id":"dQw4w9WgXcQ","context":{"album_id":3}}"#,
                Command::Play {
                    track_id: ID.into(),
                    context: ContextRef::Album(3),
                },
            ),
            (r#"{"id":1,"cmd":"pause"}"#, Command::Pause),
            (r#"{"id":1,"cmd":"resume"}"#, Command::Resume),
            (r#"{"id":1,"cmd":"toggle"}"#, Command::Toggle),
            (r#"{"id":1,"cmd":"stop"}"#, Command::Stop),
            (r#"{"id":1,"cmd":"next"}"#, Command::Next),
            (r#"{"id":1,"cmd":"prev"}"#, Command::Prev),
            (
                r#"{"id":1,"cmd":"seek","position":42.5}"#,
                Command::Seek { position: 42.5 },
            ),
            (
                r#"{"id":1,"cmd":"volume","value":55.5}"#,
                Command::Volume { value: 55.5 },
            ),
            (
                r#"{"id":1,"cmd":"loop","mode":"shuffle"}"#,
                Command::Loop {
                    mode: LoopMode::Shuffle,
                },
            ),
            (
                r#"{"id":1,"cmd":"queue.play_next","track_id":"dQw4w9WgXcQ"}"#,
                Command::QueuePlayNext {
                    track_id: ID.into(),
                },
            ),
            (
                r#"{"id":1,"cmd":"album.create","name":"Zutomayo"}"#,
                Command::AlbumCreate {
                    name: "Zutomayo".into(),
                },
            ),
            (
                r#"{"id":1,"cmd":"album.rename","album_id":3,"name":"ZTMY"}"#,
                Command::AlbumRename {
                    album_id: 3,
                    name: "ZTMY".into(),
                },
            ),
            (
                r#"{"id":1,"cmd":"album.delete","album_id":3}"#,
                Command::AlbumDelete { album_id: 3 },
            ),
            (
                r#"{"id":1,"cmd":"album.add","album_id":3,"track_id":"dQw4w9WgXcQ"}"#,
                Command::AlbumAdd {
                    album_id: 3,
                    track_id: ID.into(),
                },
            ),
            (
                r#"{"id":1,"cmd":"album.remove","album_id":3,"track_id":"dQw4w9WgXcQ"}"#,
                Command::AlbumRemove {
                    album_id: 3,
                    track_id: ID.into(),
                },
            ),
            (
                r#"{"id":1,"cmd":"album.reorder","album_id":3,"track_ids":["b","a"]}"#,
                Command::AlbumReorder {
                    album_id: 3,
                    track_ids: vec!["b".into(), "a".into()],
                },
            ),
            (
                r#"{"id":1,"cmd":"track.delete","track_id":"dQw4w9WgXcQ","to_trash":false}"#,
                Command::TrackDelete {
                    track_id: ID.into(),
                    to_trash: false,
                },
            ),
            (
                r#"{"id":1,"cmd":"track.rename","track_id":"dQw4w9WgXcQ","title":"New"}"#,
                Command::TrackRename {
                    track_id: ID.into(),
                    title: "New".into(),
                },
            ),
            (
                r#"{"id":1,"cmd":"track.retry","track_id":"dQw4w9WgXcQ"}"#,
                Command::TrackRetry {
                    track_id: ID.into(),
                },
            ),
            (r#"{"id":1,"cmd":"rescan"}"#, Command::Rescan),
            (
                r#"{"id":1,"cmd":"lyrics","track_id":"dQw4w9WgXcQ"}"#,
                Command::Lyrics {
                    track_id: ID.into(),
                },
            ),
        ];
        for (wire, cmd) in cases {
            let expected = RequestEnvelope { id: 1, cmd };
            assert_eq!(decode_request(wire).unwrap(), expected, "{wire}");
            let back = serde_json::to_value(&expected).unwrap();
            assert_eq!(back, serde_json::from_str::<Value>(wire).unwrap(), "{wire}");
        }
    }

    #[test]
    fn context_and_album_refs_use_untagged_shapes() {
        let pairs = [
            (ContextRef::Album(3), json!({"album_id": 3})),
            (ContextRef::Library, json!({"library": true})),
        ];
        for (ctx, wire) in pairs {
            assert_eq!(serde_json::to_value(&ctx).unwrap(), wire);
            assert_eq!(serde_json::from_value::<ContextRef>(wire).unwrap(), ctx);
        }
        assert!(serde_json::from_value::<ContextRef>(json!({"library": false})).is_err());

        let pairs = [
            (AlbumRef::Id { id: 3 }, json!({"id": 3})),
            (
                AlbumRef::Name {
                    name: "Inbox".into(),
                },
                json!({"name": "Inbox"}),
            ),
        ];
        for (album, wire) in pairs {
            assert_eq!(serde_json::to_value(&album).unwrap(), wire);
            assert_eq!(serde_json::from_value::<AlbumRef>(wire).unwrap(), album);
        }
    }

    #[test]
    fn add_defaults_play_true() {
        for wire in [
            r#"{"id":2,"cmd":"add","url":"u"}"#,
            r#"{"id":2,"cmd":"add","url":"u","album":null}"#,
        ] {
            assert_eq!(
                decode_request(wire).unwrap().cmd,
                Command::Add {
                    url: "u".into(),
                    album: None,
                    play: true,
                }
            );
        }
    }

    #[test]
    fn track_delete_defaults_to_trash_true() {
        let req = decode_request(r#"{"id":2,"cmd":"track.delete","track_id":"x"}"#).unwrap();
        assert_eq!(
            req.cmd,
            Command::TrackDelete {
                track_id: "x".into(),
                to_trash: true,
            }
        );
    }

    #[test]
    fn decode_invalid_json_is_bad_request_with_id_0() {
        let resp = decode_request("{not json").unwrap_err();
        assert_eq!(resp.id, 0);
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, ErrorCode::BadRequest);
    }

    #[test]
    fn decode_unknown_cmd_keeps_id() {
        let resp = decode_request(r#"{"id":7,"cmd":"frobnicate"}"#).unwrap_err();
        assert_eq!(resp.id, 7);
        assert!(!resp.ok);
        assert_eq!(resp.error.unwrap().code, ErrorCode::UnknownCommand);
    }

    #[test]
    fn events_serialize_to_exact_json() {
        let track = Track {
            id: ID.into(),
            title: ID.into(),
            uploader: None,
            duration: None,
            webpage_url: None,
            audio_path: None,
            format: None,
            file_size: None,
            added_at: Some(1),
            last_played: None,
            state: TrackState::Downloading,
            thumb_path: None,
        };
        let album = Album {
            id: 1,
            name: "Inbox".into(),
            track_ids: vec!["a".into(), "b".into()],
            created_at: 5,
            last_used_at: None,
        };
        let cases = [
            (
                Event::Player(PlayerState {
                    state: PlayState::Playing,
                    track_id: Some(ID.into()),
                    context: Some(ContextRef::Library),
                    position: 12.5,
                    at_ms: 1700000000000,
                    duration: Some(200.0),
                    volume: 80.0,
                    loop_mode: LoopMode::All,
                }),
                r#"{"event":"player","state":"playing","track_id":"dQw4w9WgXcQ","context":{"library":true},"position":12.5,"at_ms":1700000000000,"duration":200.0,"volume":80.0,"loop":"all"}"#,
            ),
            (
                Event::TrackUpsert { track },
                r#"{"event":"track.upsert","track":{"id":"dQw4w9WgXcQ","title":"dQw4w9WgXcQ","uploader":null,"duration":null,"webpage_url":null,"audio_path":null,"format":null,"file_size":null,"added_at":1,"last_played":null,"state":"downloading","thumb_path":null}}"#,
            ),
            (
                Event::TrackRemoved {
                    track_id: ID.into(),
                },
                r#"{"event":"track.removed","track_id":"dQw4w9WgXcQ"}"#,
            ),
            (
                Event::AlbumUpsert { album },
                r#"{"event":"album.upsert","album":{"id":1,"name":"Inbox","track_ids":["a","b"],"created_at":5,"last_used_at":null}}"#,
            ),
            (
                Event::AlbumRemoved { album_id: 1 },
                r#"{"event":"album.removed","album_id":1}"#,
            ),
            (
                Event::Download {
                    track_id: ID.into(),
                    phase: DownloadPhase::Downloading,
                    bytes: Some(10),
                    total: None,
                    error: None,
                },
                r#"{"event":"download","track_id":"dQw4w9WgXcQ","phase":"downloading","bytes":10,"total":null,"error":null}"#,
            ),
            (
                Event::Toast {
                    severity: Severity::Warn,
                    message: "m".into(),
                },
                r#"{"event":"toast","severity":"warn","message":"m"}"#,
            ),
            (Event::Resync, r#"{"event":"resync"}"#),
        ];
        for (event, wire) in cases {
            assert_eq!(serde_json::to_string(&event).unwrap(), wire);
        }
    }

    #[test]
    fn player_state_uses_loop_key() {
        let state = PlayerState {
            state: PlayState::Stopped,
            track_id: None,
            context: None,
            position: 0.0,
            at_ms: 0,
            duration: None,
            volume: 100.0,
            loop_mode: LoopMode::Single,
        };
        let v = serde_json::to_value(&state).unwrap();
        assert_eq!(v["loop"], json!("single"));
        assert!(v.get("loop_mode").is_none());
        assert_eq!(serde_json::from_value::<PlayerState>(v).unwrap(), state);
    }

    #[test]
    fn response_ok_omits_error_and_err_omits_result() {
        assert_eq!(
            serde_json::to_value(Response::ok(3, json!({}))).unwrap(),
            json!({"id": 3, "ok": true, "result": {}})
        );
        assert_eq!(
            serde_json::to_value(Response::err(4, ErrorCode::NotFound, "no such track")).unwrap(),
            json!({"id": 4, "ok": false, "error": {"code": "not_found", "message": "no such track"}})
        );
    }

    #[test]
    fn encode_line_ends_with_single_newline() {
        let line = encode_line(&Response::ok(1, json!({"text": "a\nb"})));
        assert_eq!(line.last(), Some(&b'\n'));
        assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
    }
}
