use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_yplay");
const URL: &str = "https://www.youtube.com/watch?v=e2eToneTest";

/// Protocol v2 worker that "downloads" the fixture in chunks.
const FAKE_WORKER: &str = r#"
import json, os, sys, threading, time

FIXTURE = sys.argv[1]
lock = threading.Lock()


def send(obj):
    with lock:
        sys.stdout.write(json.dumps(obj) + "\n")
        sys.stdout.flush()


def download(req):
    n = req["id"]
    vid = req["url"].split("v=", 1)[1][:11]
    meta = {"id": vid, "title": "E2E Tone", "uploader": "Fixture", "duration": 4,
            "webpage_url": "https://www.youtube.com/watch?v=" + vid}
    d = os.path.join(req["cache_dir"], "E2E Tone [" + vid[:8] + "]")
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, "audio.opus")
    started = False
    with open(FIXTURE, "rb") as src, open(path, "wb") as dst:
        while True:
            chunk = src.read(4096)
            if not chunk:
                break
            dst.write(chunk)
            dst.flush()
            if not started:
                started = True
                send({"id": n, "event": "started", "path": path, "dir": d, "meta": meta})
            time.sleep(0.1)
    with open(os.path.join(d, "meta.json"), "w") as f:
        json.dump(meta, f)
    send({"id": n, "ok": True, "path": path, "dir": d, "meta": meta, "thumb": None,
          "format": "opus", "file_size": os.path.getsize(path)})


send({"event": "ready", "ok": True, "protocol": 2})
while True:
    line = sys.stdin.readline()
    if not line:
        break
    req = json.loads(line)
    if req["cmd"] == "cancel":
        send({"id": req["id"], "ok": True})
    elif req["cmd"] == "download":
        threading.Thread(target=download, args=(req,), daemon=True).start()
"#;

/// Kills the server if still running and removes its socket.
struct Server {
    child: Child,
    sock: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.sock);
    }
}

fn yplay(sock: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("--socket")
        .arg(sock)
        .args(args)
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "needs mpv"]
fn serve_add_wait_cached_now_albums_and_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let script = dir.path().join("fake_worker.py");
    std::fs::write(&script, FAKE_WORKER).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tone.opus");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let sock = PathBuf::from(format!("/tmp/yp14-{}-{nanos}.sock", std::process::id()));
    let child = Command::new(BIN)
        .arg("serve")
        .arg("--dir")
        .arg(&cache)
        .arg("--socket")
        .arg(&sock)
        .env(
            "YPLAY_WORKER_CMD",
            format!(
                "/usr/bin/python3 {} {}",
                script.display(),
                fixture.display()
            ),
        )
        .env("YPLAY_MPV_EXTRA_ARGS", "--ao=null")
        // Keep the service's state dir and update check off the user's machine.
        .env("HOME", dir.path())
        .env("YPLAY_NO_UPDATE", "1")
        .spawn()
        .unwrap();
    let mut server = Server {
        child,
        sock: sock.clone(),
    };
    wait_until("the service socket", || UnixStream::connect(&sock).is_ok());

    let out = yplay(&sock, &["add", URL, "--wait"]);
    let text = stdout(&out);
    println!("{text}");
    assert!(out.status.success(), "{text}{out:?}");
    assert!(text.contains("→ Inbox"), "{text}");
    assert!(text.contains("first audio after "), "{text}");
    assert!(text.contains("downloaded in "), "{text}");

    let out = yplay(&sock, &["add", URL, "--wait"]);
    let text = stdout(&out);
    println!("{text}");
    assert!(out.status.success(), "{text}{out:?}");
    assert!(text.contains("cached"), "{text}");

    let out = yplay(&sock, &["now"]);
    let text = stdout(&out);
    println!("{text}");
    assert!(out.status.success(), "{text}{out:?}");
    assert!(text.contains("E2E Tone"), "{text}");

    let out = yplay(&sock, &["albums"]);
    let text = stdout(&out);
    println!("{text}");
    assert!(out.status.success(), "{text}{out:?}");
    assert!(text.contains("Inbox"), "{text}");

    let pid = server.child.id().to_string();
    let status = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(status.success());
    let mut exit = None;
    wait_until("the service to exit", || {
        exit = server.child.try_wait().unwrap();
        exit.is_some()
    });
    assert!(exit.unwrap().success(), "{exit:?}");
    assert!(!sock.exists(), "socket not removed");
}
