use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use super::core::Request;
use crate::protocol::{Command, ErrorCode, Event, MAX_LINE, Response, decode_request, encode_line};

/// Lines queued for one client's writer; a subscriber that falls this far
/// behind stops draining its event receiver and gets `resync`.
const WRITE_QUEUE: usize = 64;
/// Requests of one client not yet answered on the socket; at the limit the
/// connection stops reading.
const MAX_IN_FLIGHT: usize = 64;

/// A line for the writer; a reply carries its request's in-flight permit,
/// released once the line is written.
type Out = (Vec<u8>, Option<OwnedSemaphorePermit>);

enum Line {
    Complete,
    TooLong,
    Eof,
}

/// Serve one client: requests go to the core, responses and (after
/// `subscribe`) events go out through a single writer task.
pub async fn handle(stream: UnixStream, core: mpsc::UnboundedSender<Request>) {
    let (rd, wr) = stream.into_split();
    let (out, out_rx) = mpsc::channel::<Out>(WRITE_QUEUE);
    tokio::spawn(write_lines(wr, out_rx));
    let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut reader = BufReader::new(rd);
    let mut buf = Vec::new();
    let mut subscription: Option<JoinHandle<()>> = None;
    loop {
        let Ok(permit) = in_flight.clone().acquire_owned().await else {
            break;
        };
        buf.clear();
        match read_line(&mut reader, &mut buf).await {
            Line::Complete => {}
            Line::Eof => break,
            Line::TooLong => {
                let resp = Response::err(0, ErrorCode::BadRequest, "request line exceeds 1 MiB");
                let _ = out.send((encode_line(&resp), Some(permit))).await;
                break;
            }
        }
        let env = match decode_request(&String::from_utf8_lossy(&buf)) {
            Ok(env) => env,
            Err(resp) => {
                if out.send((encode_line(&resp), Some(permit))).await.is_err() {
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
            if out
                .send((encode_line(&reply.response), Some(permit)))
                .await
                .is_err()
            {
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
                    let _ = out.send((encode_line(&reply.response), Some(permit))).await;
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

async fn write_lines(mut wr: OwnedWriteHalf, mut lines: mpsc::Receiver<Out>) {
    while let Some((line, _permit)) = lines.recv().await {
        if wr.write_all(&line).await.is_err() {
            return;
        }
    }
    let _ = wr.shutdown().await;
}

async fn forward(mut events: broadcast::Receiver<Event>, out: mpsc::Sender<Out>) {
    loop {
        let line = match events.recv().await {
            Ok(ev) => encode_line(&ev),
            Err(RecvError::Lagged(_)) => encode_line(&Event::Resync),
            Err(RecvError::Closed) => return,
        };
        if out.send((line, None)).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::service::core::Reply;

    /// Resident set size of this process, in bytes.
    fn rss() -> u64 {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        let kib: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        kib * 1024
    }

    #[tokio::test]
    async fn a_client_flooding_requests_without_reading_is_throttled() {
        let (core, mut requests) = mpsc::unbounded_channel::<Request>();
        let received = Arc::new(AtomicUsize::new(0));
        let seen = received.clone();
        tokio::spawn(async move {
            // Each reply is about the size of a small library.
            let blob = "x".repeat(4096);
            while let Some(req) = requests.recv().await {
                seen.fetch_add(1, Ordering::SeqCst);
                let _ = req.reply.send(Reply {
                    response: Response::ok(req.id, json!({ "blob": blob })),
                    events: None,
                });
            }
        });
        let (client, server) = UnixStream::pair().unwrap();
        let conn = tokio::spawn(handle(server, core));
        let before = rss();

        let (rd, mut wr) = client.into_split();
        let flood = tokio::spawn(async move {
            for id in 0..20_000 {
                let line = format!("{{\"id\":{id},\"cmd\":\"library.get\"}}\n");
                if wr.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
        });
        tokio::time::sleep(Duration::from_secs(2)).await;
        let grown = rss().saturating_sub(before);
        let n = received.load(Ordering::SeqCst);
        assert!(n < 1_000, "the connection kept reading: {n} requests");
        assert!(grown < 30 << 20, "the service grew {} MiB", grown >> 20);

        flood.abort();
        drop(rd);
        tokio::time::timeout(Duration::from_secs(5), conn)
            .await
            .expect("the connection did not end")
            .unwrap();
    }
}
