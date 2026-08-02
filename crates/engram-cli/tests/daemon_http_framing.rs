//! Wave 4 — the daemon reads what the client actually sent.
//!
//! Every case here writes raw bytes at a real socket. The old handler did one
//! `read()` into a 64 KiB buffer and treated whatever arrived as the whole
//! request, so a body in two segments was truncated and blamed on the caller's
//! JSON, and one slow client blocked every other.

mod daemon_support;

use daemon_support::*;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

fn seeded() -> Store {
    let store = Store::new();
    store.seed(&[
        "remember",
        "Ada wrote the first algorithm.",
        "--kb",
        "default",
    ]);
    store
}

#[test]
fn body_split_across_writes_is_reassembled() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    let body = r#"{"query":"algorithm","profile":"offline","kb":"default"}"#;
    let mut stream = daemon.connect();
    write!(
        stream,
        "POST /v1/recall HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
    std::thread::sleep(Duration::from_millis(50));
    // Three chunks, with pauses. This is the defect: the old parser saw only
    // whatever happened to be in the first read.
    for chunk in [&body[..20], &body[20..40], &body[40..]] {
        stream.write_all(chunk.as_bytes()).unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let (status, body) = parse_response(&raw);
    assert_eq!(status, 200, "{body}");
    assert!(body["data"]["results"].is_array(), "{body}");
}

#[test]
fn body_larger_than_64k_is_not_truncated() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    // A 200 KiB query: four times the old buffer.
    let query = "algorithm ".repeat(20_000);
    let body =
        serde_json::json!({ "query": query, "profile": "offline", "kb": "default" }).to_string();
    assert!(body.len() > 64 * 1024);
    let (status, parsed) = daemon.post("/v1/recall", &body);
    assert_eq!(status, 200, "a 200 KiB body must parse whole: {parsed}");
}

#[test]
fn short_body_is_incomplete_not_malformed_json() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    let mut stream = daemon.connect();
    write!(
        stream,
        "POST /v1/recall HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: 100\r\n\
         connection: close\r\n\r\n"
    )
    .unwrap();
    stream.write_all(&b"{\"query\":\"x\""[..]).unwrap();
    stream.flush().unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let (status, body) = parse_response(&raw);
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        body["error"]["code"], "incomplete_body",
        "truncation must be named, not reported as bad JSON: {body}"
    );
}

#[test]
fn chunked_is_501() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    let raw = daemon
        .raw(
            b"POST /v1/recall HTTP/1.1\r\nhost: 127.0.0.1\r\ntransfer-encoding: chunked\r\n\
              connection: close\r\n\r\n0\r\n\r\n",
        )
        .unwrap();
    let (status, body) = parse_response(&raw);
    assert_eq!(status, 501, "{body}");
    assert_eq!(body["error"]["code"], "unsupported_transfer_encoding");
}

#[test]
fn duplicate_conflicting_content_length_is_400() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    let raw = daemon
        .raw(
            b"POST /v1/recall HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: 2\r\n\
              content-length: 9\r\nconnection: close\r\n\r\n{}",
        )
        .unwrap();
    let (status, body) = parse_response(&raw);
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "bad_content_length");
}

#[test]
fn body_over_limit_is_413_without_reading_it() {
    let store = seeded();
    let daemon = Daemon::start_with(&store, &[("ENGRAM_HTTP_MAX_BODY_BYTES", "1024")]);
    let mut stream = daemon.connect();
    write!(
        stream,
        "POST /v1/recall HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: 9000000\r\n\
         connection: close\r\n\r\n"
    )
    .unwrap();
    stream.flush().unwrap();
    // The client never sends the body; the refusal must still arrive.
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let (status, body) = parse_response(&raw);
    assert_eq!(status, 413, "{body}");
    assert_eq!(body["error"]["code"], "body_too_large");
}

#[test]
fn header_flood_is_431() {
    let store = seeded();
    let daemon = Daemon::start_with(&store, &[("ENGRAM_HTTP_MAX_HEADER_BYTES", "2048")]);
    let mut req = String::from("GET /health HTTP/1.1\r\nhost: 127.0.0.1\r\n");
    for i in 0..500 {
        req.push_str(&format!("x-pad-{i}: {}\r\n", "y".repeat(64)));
    }
    req.push_str("\r\n");
    let raw = daemon.raw(req.as_bytes()).unwrap();
    let (status, body) = parse_response(&raw);
    assert_eq!(status, 431, "{body}");
    assert_eq!(body["error"]["code"], "header_too_large");
}

#[test]
fn invalid_utf8_body_is_400_never_lossily_accepted() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    let mut bytes =
        b"POST /v1/recall HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: 2\r\nconnection: close\r\n\r\n"
            .to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe]);
    let raw = daemon.raw(&bytes).unwrap();
    let (status, body) = parse_response(&raw);
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "malformed_json");
}

#[test]
fn expect_100_continue_is_answered() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    let body = r#"{"query":"algorithm","profile":"offline","kb":"default"}"#;
    let mut stream = daemon.connect();
    write!(
        stream,
        "POST /v1/recall HTTP/1.1\r\nhost: 127.0.0.1\r\nexpect: 100-continue\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
    let mut head = [0u8; 25];
    stream.read_exact(&mut head).unwrap();
    assert!(
        String::from_utf8_lossy(&head).starts_with("HTTP/1.1 100"),
        "without the interim response curl waits a full second on every request: {}",
        String::from_utf8_lossy(&head)
    );
    stream.write_all(body.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).unwrap();
    assert!(String::from_utf8_lossy(&rest).contains("200 OK"));
}

#[test]
fn keep_alive_serves_two_requests_on_one_connection() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    let mut stream = daemon.connect();
    let req = "GET /health HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n";
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();
    let first = read_one_response(&mut stream);
    assert!(first.contains("200 OK"), "{first}");
    assert!(
        first.to_lowercase().contains("connection: keep-alive"),
        "{first}"
    );
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();
    let second = read_one_response(&mut stream);
    assert!(
        second.contains("200 OK"),
        "second request on the same socket: {second}"
    );
}

/// Read exactly one HTTP response: head, then `content-length` bytes. Reading
/// "whatever one read() returns" is the assumption this whole file exists to
/// disprove, and the harness must not make it either.
fn read_one_response(stream: &mut std::net::TcpStream) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    let head_end = loop {
        let n = stream.read(&mut byte).expect("read");
        assert!(n == 1, "connection closed mid-response");
        buf.push(byte[0]);
        if buf.len() >= 4 && &buf[buf.len() - 4..] == b"\r\n\r\n" {
            break buf.len();
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_lowercase();
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).expect("body");
    buf.extend_from_slice(&body);
    String::from_utf8_lossy(&buf).to_string()
}

#[test]
fn a_slow_client_does_not_block_another() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    // One connection sends a header line and stalls, holding the socket open.
    let mut stalled = daemon.connect();
    stalled
        .write_all(b"POST /v1/recall HTTP/1.1\r\nhost: 127.0.0.1\r\n")
        .unwrap();
    stalled.flush().unwrap();

    let began = Instant::now();
    let (status, body) = daemon.get("/health");
    let elapsed = began.elapsed();
    assert_eq!(status, 200, "{body}");
    assert!(
        elapsed < Duration::from_secs(2),
        "a stalled connection blocked the accept loop for {elapsed:?}"
    );
    drop(stalled);
}

#[test]
fn twenty_concurrent_recalls_all_answer() {
    let store = seeded();
    let daemon = std::sync::Arc::new(Daemon::start(&store));
    let mut handles = Vec::new();
    for _ in 0..20 {
        let d = daemon.clone();
        handles.push(std::thread::spawn(move || {
            d.post(
                "/v1/recall",
                r#"{"query":"algorithm","profile":"offline","kb":"default"}"#,
            )
        }));
    }
    for h in handles {
        let (status, body) = h.join().expect("thread");
        assert_eq!(
            status, 200,
            "no request may report a locked database: {body}"
        );
    }
}

#[test]
fn over_the_connection_cap_is_503_not_a_hang() {
    let store = seeded();
    let daemon = Daemon::start_with(&store, &[("ENGRAM_HTTP_MAX_CONNECTIONS", "2")]);
    // Hold both permits with connections that send nothing.
    let held: Vec<_> = (0..2)
        .map(|_| {
            let mut s = daemon.connect();
            let _ = s.write_all(b"GET /health HTTP/1.1\r\n");
            let _ = s.flush();
            s
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    let mut extra = daemon.connect();
    extra
        .write_all(b"GET /health HTTP/1.1\r\nhost: x\r\n\r\n")
        .unwrap();
    extra.flush().unwrap();
    let mut raw = Vec::new();
    extra.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw);
    assert!(
        text.contains("503") && text.to_lowercase().contains("retry-after"),
        "over the cap must be a refusal with a retry hint, not silence: {text}"
    );
    drop(held);
}

#[test]
fn a_client_that_sends_nothing_times_out() {
    let store = seeded();
    let daemon = Daemon::start_with(&store, &[("ENGRAM_HTTP_MAX_CONNECTIONS", "8")]);
    let mut stream = daemon.connect();
    stream
        .set_read_timeout(Some(Duration::from_secs(45)))
        .unwrap();
    // Send a partial head so the header timeout (not the idle timeout) applies.
    stream.write_all(b"GET /health HTTP/1.1\r\n").unwrap();
    stream.flush().unwrap();
    let mut raw = Vec::new();
    let began = Instant::now();
    stream.read_to_end(&mut raw).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(40),
        "a half-open request must not be held forever"
    );
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("408") || text.is_empty(), "{text}");
}

#[test]
fn a_bad_request_line_keeps_its_envelope() {
    let store = seeded();
    let daemon = Daemon::start(&store);
    let raw = daemon.raw(b"garbage\r\n\r\n").unwrap();
    let (status, body) = parse_response(&raw);
    assert_eq!(status, 400);
    assert_eq!(body["error"]["code"], "bad_request");
    assert_eq!(body["error"]["message"], "bad request");
    assert_eq!(body["error"]["exit_code"], 3);
}

#[test]
fn an_unknown_endpoint_is_still_400() {
    // Compatibility pin: `route`'s fallback has always raised BadInput and the
    // wave-0.5 daemon tests assert on that envelope. A missing RESOURCE is the
    // 404 case; a path nobody implemented is not.
    let store = seeded();
    let daemon = Daemon::start(&store);
    let (status, body) = daemon.get("/v1/nope");
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "bad_input");
}

#[test]
fn a_token_prefix_does_not_authorize() {
    let store = Store::new();
    store.seed(&[
        "remember",
        "Ada wrote the first algorithm.",
        "--kb",
        "default",
    ]);
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        drop(l);
        p
    };
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_engram"))
        .env("ENGRAM_STORE", store.path())
        .env("ENGRAM_SLEEP_COMPILE", "0")
        .args([
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--token",
            "s3cret",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let mut ready = false;
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ready, "daemon never bound");

    let ask = |auth: Option<&str>| -> u16 {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut req = String::from("GET /health HTTP/1.1\r\nhost: 127.0.0.1\r\n");
        if let Some(a) = auth {
            req.push_str(&format!("authorization: {a}\r\n"));
        }
        req.push_str("connection: close\r\n\r\n");
        s.write_all(req.as_bytes()).unwrap();
        s.flush().unwrap();
        let mut raw = Vec::new();
        let _ = s.read_to_end(&mut raw);
        status_of(&raw)
    };
    assert_eq!(ask(None), 401, "no header is unauthorized");
    assert_eq!(ask(Some("Bearer s3cre")), 401, "a prefix is not the token");
    assert_eq!(
        ask(Some("Bearer s3crets")),
        401,
        "an extension is not the token"
    );
    assert_eq!(ask(Some("Bearer s3cret")), 200, "the token itself works");
    let _ = child.kill();
    let _ = child.wait();
}
