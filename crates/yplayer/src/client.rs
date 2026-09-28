use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::Instant;

use crate::protocol::{
    AddResult, AlbumRef, Command, ContextRef, DownloadPhase, Event, PlayState, PlayerState,
    RequestEnvelope, Response, encode_line,
};
use crate::types::{Album, Track, TrackState};

/// Timeout for one request/response exchange.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long `add --wait` waits for the download to finish.
const WAIT_TIMEOUT: Duration = Duration::from_secs(600);

pub const NOT_RUNNING: &str = "yplay service is not running — start it with: yplay serve";

#[derive(Debug)]
pub enum ClientError {
    /// The socket refused the connection (exit 2).
    NotRunning,
    /// The service returned an error, or the exchange failed (exit 1).
    Failed(String),
}

impl ClientError {
    pub fn exit_code(&self) -> i32 {
        match self {
            ClientError::NotRunning => 2,
            ClientError::Failed(_) => 1,
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NotRunning => f.write_str(NOT_RUNNING),
            ClientError::Failed(msg) => f.write_str(msg),
        }
    }
}

fn failed(msg: impl std::fmt::Display) -> ClientError {
    ClientError::Failed(msg.to_string())
}

fn decode<T: DeserializeOwned>(v: Value) -> Result<T, ClientError> {
    serde_json::from_value(v).map_err(|e| failed(format!("unexpected reply: {e}")))
}

enum Incoming {
    Response(Response),
    Event(Event),
}

/// One connection to `yplay serve`.
pub struct Conn {
    lines: Lines<BufReader<OwnedReadHalf>>,
    wr: OwnedWriteHalf,
    next_id: u64,
    /// Events read while waiting for a response, with their arrival time.
    events: VecDeque<(Instant, Event)>,
}

impl Conn {
    pub async fn connect(socket: &Path) -> Result<Conn, ClientError> {
        let stream = UnixStream::connect(socket)
            .await
            .map_err(|_| ClientError::NotRunning)?;
        let (rd, wr) = stream.into_split();
        Ok(Conn {
            lines: BufReader::new(rd).lines(),
            wr,
            next_id: 1,
            events: VecDeque::new(),
        })
    }

    async fn read(&mut self, deadline: Instant) -> Result<Incoming, ClientError> {
        let line = tokio::time::timeout_at(deadline, self.lines.next_line())
            .await
            .map_err(|_| failed("timed out waiting for the yplay service"))?
            .map_err(failed)?
            .ok_or_else(|| failed("the yplay service closed the connection"))?;
        let v: Value = serde_json::from_str(&line).map_err(failed)?;
        if v.get("event").is_some() {
            Ok(Incoming::Event(decode(v)?))
        } else {
            Ok(Incoming::Response(decode(v)?))
        }
    }

    /// Send `cmd` and return its result; events that arrive first are queued.
    pub async fn call(&mut self, cmd: Command) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        self.wr
            .write_all(&encode_line(&RequestEnvelope { id, cmd }))
            .await
            .map_err(failed)?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        loop {
            match self.read(deadline).await? {
                Incoming::Event(ev) => self.events.push_back((Instant::now(), ev)),
                Incoming::Response(resp) if resp.id == id => {
                    if resp.ok {
                        return Ok(resp.result.unwrap_or(Value::Null));
                    }
                    return Err(ClientError::Failed(
                        resp.error
                            .map_or_else(|| "request failed".into(), |e| e.message),
                    ));
                }
                Incoming::Response(_) => {}
            }
        }
    }

    /// Next event (queued ones first) and when it arrived.
    async fn next_event(&mut self, deadline: Instant) -> Result<(Instant, Event), ClientError> {
        if let Some(queued) = self.events.pop_front() {
            return Ok(queued);
        }
        loop {
            if let Incoming::Event(ev) = self.read(deadline).await? {
                return Ok((Instant::now(), ev));
            }
        }
    }
}

#[derive(Deserialize)]
struct Library {
    tracks: Vec<Track>,
    albums: Vec<Album>,
}

async fn library(conn: &mut Conn) -> Result<Library, ClientError> {
    decode(conn.call(Command::LibraryGet).await?)
}

/// Send a command whose result is `{}`.
pub async fn send(socket: &Path, cmd: Command) -> Result<(), ClientError> {
    Conn::connect(socket).await?.call(cmd).await?;
    Ok(())
}

pub struct AddOptions {
    pub url: String,
    pub album: Option<String>,
    pub play: bool,
    pub wait: bool,
}

/// `yplay add`: prints `Added <title> → <album>`; with `wait`, also the time
/// to first audio and to the finished download.
pub async fn add(socket: &Path, opts: AddOptions) -> Result<(), ClientError> {
    let mut conn = Conn::connect(socket).await?;
    if opts.wait {
        conn.call(Command::Subscribe).await?;
    }
    let sent = Instant::now();
    let added: AddResult = decode(
        conn.call(Command::Add {
            url: opts.url,
            album: opts.album.map(|name| AlbumRef::Name { name }),
            play: opts.play,
        })
        .await?,
    )?;
    let lib = library(&mut conn).await?;
    let track = lib.tracks.iter().find(|t| t.id == added.track_id);
    let title = track.map_or(added.track_id.as_str(), |t| t.title.as_str());
    let album = lib
        .albums
        .iter()
        .find(|a| a.id == added.album_id)
        .map_or("?", |a| a.name.as_str());
    println!("Added {title} → {album}");
    if !opts.wait {
        return Ok(());
    }
    if !added.was_new && track.is_some_and(|t| t.state == TrackState::Complete) {
        println!(
            "{}",
            if opts.play {
                "cached — playing"
            } else {
                "cached"
            }
        );
        return Ok(());
    }
    let deadline = sent + WAIT_TIMEOUT;
    let mut heard = false;
    loop {
        let (at, ev) = conn.next_event(deadline).await?;
        let secs = (at - sent).as_secs_f64();
        match ev {
            Event::Player(p)
                if opts.play
                    && !heard
                    && p.state == PlayState::Playing
                    && p.track_id.as_deref() == Some(added.track_id.as_str()) =>
            {
                heard = true;
                println!("first audio after {secs:.1}s");
            }
            Event::Download {
                track_id,
                phase,
                error,
                ..
            } if track_id == added.track_id => match phase {
                DownloadPhase::Done => {
                    println!("downloaded in {secs:.1}s");
                    return Ok(());
                }
                DownloadPhase::Failed => {
                    return Err(failed(format!(
                        "download failed: {}",
                        error.unwrap_or_default()
                    )));
                }
                DownloadPhase::Cancelled => return Err(failed("download cancelled")),
                DownloadPhase::Fetching | DownloadPhase::Downloading => {}
            },
            _ => {}
        }
    }
}

/// `yplay play`: play a track in an album (by name) or the library.
pub async fn play(
    socket: &Path,
    track_id: String,
    album: Option<String>,
) -> Result<(), ClientError> {
    let mut conn = Conn::connect(socket).await?;
    let context = match album {
        None => ContextRef::Library,
        Some(name) => {
            let lib = library(&mut conn).await?;
            let album = lib
                .albums
                .iter()
                .find(|a| a.name == name)
                .ok_or_else(|| failed(format!("no such album: {name}")))?;
            ContextRef::Album(album.id)
        }
    };
    conn.call(Command::Play { track_id, context }).await?;
    Ok(())
}

#[derive(Deserialize)]
struct Now {
    player: PlayerState,
    track: Option<Track>,
}

/// `yplay now`: `▶ Title — Uploader  m:ss / m:ss  [album]`.
pub async fn now(socket: &Path) -> Result<(), ClientError> {
    let mut conn = Conn::connect(socket).await?;
    let Now { player, track } = decode(conn.call(Command::Now).await?)?;
    let icon = match player.state {
        PlayState::Playing => "▶",
        PlayState::Paused => "⏸",
        PlayState::Stopped => "■",
    };
    let Some(track) = track else {
        println!("{icon}");
        return Ok(());
    };
    let album = match player.context {
        Some(ContextRef::Album(id)) => library(&mut conn)
            .await?
            .albums
            .into_iter()
            .find(|a| a.id == id)
            .map(|a| a.name),
        Some(ContextRef::Library) => Some("Library".to_string()),
        None => None,
    };
    let mut position = player.position;
    if player.state == PlayState::Playing {
        position += (now_ms() - player.at_ms).max(0) as f64 / 1000.0;
    }
    let duration = player.duration.or(track.duration.map(|d| d as f64));
    let mut line = format!("{icon} {}", track.title);
    if let Some(uploader) = &track.uploader {
        line.push_str(&format!(" — {uploader}"));
    }
    line.push_str(&format!("  {} / {}", mmss(Some(position)), mmss(duration)));
    if let Some(album) = album {
        line.push_str(&format!("  [{album}]"));
    }
    println!("{line}");
    Ok(())
}

/// `yplay albums`: `name  (n songs)` per album.
pub async fn albums(socket: &Path) -> Result<(), ClientError> {
    let mut conn = Conn::connect(socket).await?;
    for album in library(&mut conn).await?.albums {
        println!("{}  ({} songs)", album.name, album.track_ids.len());
    }
    Ok(())
}

fn mmss(secs: Option<f64>) -> String {
    match secs {
        Some(s) => {
            let s = s.max(0.0) as i64;
            format!("{}:{:02}", s / 60, s % 60)
        }
        None => "?:??".to_string(),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
