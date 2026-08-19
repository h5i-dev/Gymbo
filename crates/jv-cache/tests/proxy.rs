//! `settings.xml` `<proxies>` reach the wire.
//!
//! The bug this exists for was not a wrong proxy — it was a proxy that parsed
//! cleanly and was never applied, so a machine that could only reach Central
//! through a corporate proxy saw connection failures with nothing in the output
//! mentioning the proxy it had configured. A unit test over the selector cannot
//! catch that: the selector was right, and nothing called it.
//!
//! So this test asserts against a real socket. A forward proxy for plain HTTP
//! is small enough to write here — the client sends the request line in
//! absolute form, `GET http://host/path HTTP/1.1`, which is exactly the thing
//! that proves the request went *through* the proxy rather than direct.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use jv_cache::{HttpTransport, Transport};
use jv_model::{Proxy, Settings};
use jv_repo::{Credentials, ProxySelector};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// What the fake proxy saw.
#[derive(Default)]
struct Seen {
    requests: AtomicUsize,
    /// Set when a request arrived in absolute form, which only a proxy gets.
    absolute_form: AtomicUsize,
}

/// Starts a forward proxy that answers everything with `body`, and reports what
/// it was asked for. Returns its address.
async fn proxy(seen: Arc<Seen>, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr").to_string();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).await.is_err() {
                    return;
                }
                seen.requests.fetch_add(1, Ordering::SeqCst);
                // `GET http://host/path HTTP/1.1` — origin-form would be
                // `GET /path HTTP/1.1`, which is what a direct connection sends.
                if request_line
                    .split_whitespace()
                    .nth(1)
                    .is_some_and(|target| target.starts_with("http://"))
                {
                    seen.absolute_form.fetch_add(1, Ordering::SeqCst);
                }
                // Drain the rest of the headers so the client is not writing
                // into a socket nobody reads.
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) if line.trim().is_empty() => break,
                        Ok(_) => {}
                        Err(_) => return,
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = reader.into_inner().write_all(response.as_bytes()).await;
            });
        }
    });

    address
}

fn settings_with_proxy(host_port: &str, non_proxy_hosts: Option<&str>) -> Settings {
    let (host, port) = host_port.rsplit_once(':').expect("host:port");
    Settings {
        proxies: vec![Proxy {
            id: Some("test".to_owned()),
            active: None,
            protocol: Some("http".to_owned()),
            host: Some(host.to_owned()),
            port: Some(port.to_owned()),
            username: None,
            password: None,
            non_proxy_hosts: non_proxy_hosts.map(str::to_owned),
        }],
        ..Settings::default()
    }
}

#[tokio::test]
async fn a_configured_proxy_carries_the_request() {
    let seen = Arc::new(Seen::default());
    let address = proxy(Arc::clone(&seen), "through-the-proxy").await;

    let settings = settings_with_proxy(&address, None);
    let transport = HttpTransport::with_proxies(ProxySelector::from_settings(&settings))
        .expect("a client with a proxy");

    // A host that does not resolve: if the proxy were ignored, this could not
    // possibly succeed, which is what makes the assertion meaningful rather
    // than incidental.
    let body = transport
        .get(
            "http://repo.invalid.example/g/a/1/a-1.pom",
            &Credentials::default(),
        )
        .await
        .expect("the request should succeed through the proxy")
        .expect("the proxy answered 200, so there are bytes");

    assert_eq!(body, b"through-the-proxy");
    assert_eq!(
        seen.absolute_form.load(Ordering::SeqCst),
        1,
        "the proxy did not receive an absolute-form request, so the client \
         connected directly"
    );
}

#[tokio::test]
async fn non_proxy_hosts_keeps_the_request_off_the_proxy() {
    let seen = Arc::new(Seen::default());
    let address = proxy(Arc::clone(&seen), "should-not-be-reached").await;

    // The same proxy, but this host is excluded from it.
    let settings = settings_with_proxy(&address, Some("*.invalid.example"));
    let transport = HttpTransport::with_proxies(ProxySelector::from_settings(&settings))
        .expect("a client with a proxy");

    // Direct to a host that does not exist, so this must fail rather than be
    // quietly rescued by the proxy.
    let result = transport
        .get(
            "http://repo.invalid.example/g/a/1/a-1.pom",
            &Credentials::default(),
        )
        .await;

    assert!(
        result.is_err(),
        "an excluded host must connect directly and fail, not fall back to the proxy"
    );
    assert_eq!(
        seen.requests.load(Ordering::SeqCst),
        0,
        "an excluded host still reached the proxy"
    );
}

#[tokio::test]
async fn no_configured_proxy_leaves_the_client_alone() {
    // Nothing configured: the selector is empty, and the builder is untouched
    // so reqwest's own environment handling still applies.
    let selector = ProxySelector::from_settings(&Settings::default());
    assert!(selector.is_empty());
    assert!(HttpTransport::with_proxies(selector).is_ok());
}
