//! An anonymous session keeps the cookies YouTube sets.
//!
//! The reference client's watch-page GET carries one cookie, `VISITOR_INFO1_LIVE`, set by
//! an earlier response (hover capture). A watch page fetched without it carries no
//! `animatedThumbnailOverlayViewModel` entries. A local one-shot server stands in for
//! YouTube.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use innertube::{ClientConfig, Session};

/// Serve two requests, recording the `Cookie` header of each, and set a cookie on the first.
fn cookie_echo_server() -> (String, std::thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = std::thread::spawn(move || {
        let mut seen = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(stream);
            let mut cookie = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line.trim().is_empty() {
                    break;
                }
                if let Some(v) = line.strip_prefix("cookie: ").or_else(|| line.strip_prefix("Cookie: ")) {
                    cookie = v.trim().to_string();
                }
            }
            seen.push(cookie);
            let body = "ok";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\
                 Set-Cookie: VISITOR_INFO1_LIVE=abc123; Path=/\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = reader.get_mut().write_all(resp.as_bytes());
            let _ = reader.get_mut().flush();
        }
        seen
    });
    (format!("http://{addr}/"), handle)
}

#[tokio::test(flavor = "current_thread")]
async fn anonymous_session_keeps_the_cookies_the_server_sets() {
    let (url, server) = cookie_echo_server();
    let session = Session::new(ClientConfig::web()).expect("session");

    session.get_text(&url).await.expect("first request");
    session.get_text(&url).await.expect("second request");

    let seen = server.join().expect("server thread");
    assert_eq!(seen.len(), 2);

    // The consent cookie is scoped to `.youtube.com`.
    assert!(!seen[0].contains("SOCS"), "consent cookie leaked to another host: {:?}", seen[0]);

    assert!(
        seen[1].contains("VISITOR_INFO1_LIVE=abc123"),
        "the cookie the server set was not sent back: {:?}",
        seen[1]
    );
}
