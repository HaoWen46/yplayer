use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::core::Request;
use crate::protocol::{Command, ErrorCode, Event, MAX_LINE, Response, decode_request, encode_line};

/// Lines queued for one client's writer; a subscriber that falls this far
/// behind stops draining its event receiver and gets `resync`.
const WRITE_QUEUE: usize = 64;

enum Line {
    Complete,
    TooLong,
    Eof,
}

/// Serve one client: requests go to the core, responses and (after
/// `subscribe`) events go out through a single writer task.
pub async fn handle(stream: UnixStream, core: mpsc::UnboundedSender<Request>) {
    let (rd, wr) = stream.into_split();
    let (out, out_rx) = mpsc::channel(WRITE_QUEUE);
    tokio::spawn(write_lines(wr, out_rx));
    let mut reader = BufReader::new(rd);
    let mut buf = Vec::new();
    let mut subscription: Option<JoinHandle<()>> = None;
    loop {
        buf.clear();
        match read_line(&mut reader, &mut buf).await {
            Line::Complete => {}
            Line::Eof => break,
            Line::TooLong => {
                let resp = Response::err(0, ErrorCode::BadRequest, "request line exceeds 1 MiB");
                let _ = out.send(encode_line(&resp)).await;
                break;
            }
        }
        let env = match decode_request(&String::from_utf8_lossy(&buf)) {
            Ok(env) => env,
            Err(resp) => {
                if out.send(encode_line(&resp)).await.is_err() {
                    break;
                }
                continue;
            }
        };
        let subscribe = env.cmd == Command::Subscribe;
        let (reply, rx) = oneshot::channel();
        let req = Request {
            id: env.id,
            cmd: env.cmd,
            reply,
        };
        if core.send(req).is_err() {
            break;
        }
        if subscribe {
            // Answered inline so the snapshot is written before any event.
            let Ok(reply) = rx.await else { break };
            if out.send(encode_line(&reply.response)).await.is_err() {
                break;
            }
            if let Some(events) = reply.events {
                if let Some(old) = subscription.take() {
                    old.abort();
                }
                subscription = Some(tokio::spawn(forward(events, out.clone())));
            }
        } else {
            let out = out.clone();
            tokio::spawn(async move {
                if let Ok(reply) = rx.await {
                    let _ = out.send(encode_line(&reply.response)).await;
                }
            });
        }
    }
    if let Some(sub) = subscription {
        sub.abort();
    }
}

/// Read one `\n`-terminated line into `buf` (without the `\n`), reading at
/// most `MAX_LINE + 1` bytes.
async fn read_line(reader: &mut BufReader<OwnedReadHalf>, buf: &mut Vec<u8>) -> Line {
    let limit = MAX_LINE as u64 + 1;
    match (&mut *reader).take(limit).read_until(b'\n', buf).await {
        Ok(0) | Err(_) => Line::Eof,
        Ok(_) if buf.last() == Some(&b'\n') => {
            buf.pop();
            Line::Complete
        }
        Ok(_) if buf.len() > MAX_LINE => Line::TooLong,
        Ok(_) => Line::Complete,
    }
}

async fn write_lines(mut wr: OwnedWriteHalf, mut lines: mpsc::Receiver<Vec<u8>>) {
    while let Some(line) = lines.recv().await {
        if wr.write_all(&line).await.is_err() {
            return;
        }
    }
    let _ = wr.shutdown().await;
}

async fn forward(mut events: broadcast::Receiver<Event>, out: mpsc::Sender<Vec<u8>>) {
    loop {
        let line = match events.recv().await {
            Ok(ev) => encode_line(&ev),
            Err(RecvError::Lagged(_)) => encode_line(&Event::Resync),
            Err(RecvError::Closed) => return,
        };
        if out.send(line).await.is_err() {
            return;
        }
    }
}
