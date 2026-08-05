//! Isolation harness for the daemon-jobs lane.
//!
//! Every test here points `ENGRAM_STORE` at a `TempDir` and nothing else. No
//! `point_at_home`, no XDG tricks: a suite that isolates by HOME has already
//! failed twice in this repo — once by migrating the developer's live store
//! and bricking the installed binary against its own database.
//!
//! Provider keys are cleared on every spawned process too, so a developer's
//! real keys cannot make a test spend money or reach the network.

#![allow(dead_code)]

use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::Duration;
use tempfile::TempDir;

pub struct Store {
    pub dir: TempDir,
}

impl Store {
    pub fn new() -> Store {
        Store {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn db(&self) -> PathBuf {
        self.dir.path().join("db.sqlite")
    }

    pub fn cmd(&self, bin: &str) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin(bin).expect("binary built");
        cmd.env("ENGRAM_STORE", self.dir.path())
            .env_remove("GEMINI_API_KEY")
            .env_remove("COHERE_API_KEY")
            .env_remove("OPENROUTER_API_KEY");
        cmd
    }

    pub fn engram(&self) -> assert_cmd::Command {
        self.cmd("engram")
    }

    /// A seeding call through the real binary with the bench stub forced on.
    pub fn seed(&self, args: &[&str]) -> Value {
        let mut cmd = self.engram();
        cmd.env("ENGRAM_BENCH_FORCE_STUB", "1");
        let out = cmd.args(args).arg("--json").output().expect("engram runs");
        assert!(
            out.status.success(),
            "seed {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
    }

    pub fn open(&self) -> engram_storage::SqliteStore {
        engram_storage::SqliteStore::open(self.db()).expect("open store")
    }
}

impl Default for Store {
    fn default() -> Self {
        Store::new()
    }
}

/// Run a command and parse its stdout as the JSON envelope.
pub fn run_json(cmd: &mut assert_cmd::Command, args: &[&str]) -> (bool, Value) {
    let out = cmd.args(args).arg("--json").output().expect("engram runs");
    let parsed = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| serde_json::json!({ "stderr": String::from_utf8_lossy(&out.stderr) }));
    (out.status.success(), parsed)
}

pub fn exit_code(cmd: &mut assert_cmd::Command, args: &[&str]) -> i32 {
    let out = cmd.args(args).arg("--json").output().expect("engram runs");
    out.status.code().unwrap_or(-1)
}

pub struct Daemon {
    child: Child,
    pub port: u16,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A port is probed and released before the daemon binds it, so under `cargo
/// test`'s parallelism another test can take it in between. Losing that race is
/// not a product failure: take a fresh port and try again.
fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
    let port = l.local_addr().expect("addr").port();
    drop(l);
    port
}

impl Daemon {
    pub fn start(store: &Store) -> Daemon {
        Daemon::start_with(store, &[])
    }

    pub fn start_with(store: &Store, env: &[(&str, &str)]) -> Daemon {
        for _ in 0..5 {
            if let Some(d) = Daemon::try_start(store, env) {
                return d;
            }
        }
        panic!("engram serve never became healthy on any of 5 ports");
    }

    fn try_start(store: &Store, env: &[(&str, &str)]) -> Option<Daemon> {
        let port = free_port();
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_engram"));
        cmd.env("ENGRAM_STORE", store.dir.path())
            .env_remove("GEMINI_API_KEY")
            .env_remove("COHERE_API_KEY")
            .env_remove("OPENROUTER_API_KEY")
            .env_remove("ENGRAM_BENCH_FORCE_STUB")
            // Off unless a test asks for it: a background compile firing in the
            // middle of an unrelated assertion is a flake generator.
            .env("ENGRAM_SLEEP_COMPILE", "0");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = cmd
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn engram serve");
        let daemon = Daemon { child, port };
        for _ in 0..100 {
            if let Ok((200, _)) = daemon.try_get("/health") {
                return Some(daemon);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    pub fn connect(&self) -> TcpStream {
        let s = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        s
    }

    pub fn get(&self, path: &str) -> (u16, Value) {
        self.request("GET", path, None).expect("daemon answers")
    }

    pub fn post(&self, path: &str, body: &str) -> (u16, Value) {
        self.request("POST", path, Some(body))
            .expect("daemon answers")
    }

    fn try_get(&self, path: &str) -> std::io::Result<(u16, Value)> {
        self.request("GET", path, None)
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> std::io::Result<(u16, Value)> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port))?;
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        let body = body.unwrap_or("");
        let head = format!(
            "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        // Head and body in two writes: the daemon used to assume one read()
        // held both, and this harness must not be able to hide that.
        stream.write_all(head.as_bytes())?;
        stream.flush()?;
        if !body.is_empty() {
            stream.write_all(body.as_bytes())?;
            stream.flush()?;
        }
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw)?;
        Ok(parse_response(&raw))
    }

    /// Raw bytes in, raw bytes out. For the framing tests.
    pub fn raw(&self, bytes: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut stream = self.connect();
        stream.write_all(bytes)?;
        stream.flush()?;
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw);
        Ok(raw)
    }
}

pub fn parse_response(raw: &[u8]) -> (u16, Value) {
    let text = String::from_utf8_lossy(raw);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

pub fn status_of(raw: &[u8]) -> u16 {
    parse_response(raw).0
}
