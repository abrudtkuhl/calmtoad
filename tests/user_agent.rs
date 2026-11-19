//! The crawler identifies itself as the running release on every request it makes, and site
//! owners can target it by that name in robots.txt.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;

use calmtoad::crawl::{CrawlOptions, Limits, crawl_site_with};
use calmtoad::render::RenderMode;

type Seen = Arc<Mutex<HashMap<String, Vec<String>>>>;

fn serve(stream: TcpStream, base: String, seen: Seen) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(_) => return,
    });
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();
    let mut user_agent = String::new();
    let mut line = String::new();
    while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("user-agent")
        {
            user_agent = value.trim().to_string();
        }
        line.clear();
    }
    seen.lock()
        .unwrap()
        .entry(path.clone())
        .or_default()
        .push(user_agent);

    let (status, content_type, body) = match path.as_str() {
        // Everyone else is shut out; only a crawler that names itself CalmToad may crawl,
        // and it may not enter /private.
        "/robots.txt" => (
            "200 OK",
            "text/plain",
            format!(
                "User-agent: *\nDisallow: /\n\nUser-agent: CalmToad\nDisallow: /private\n\nSitemap: {base}/sitemap.xml\n"
            ),
        ),
        "/sitemap.xml" => (
            "200 OK",
            "application/xml",
            format!(
                "<?xml version=\"1.0\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\"><url><loc>{base}/listed</loc></url></urlset>"
            ),
        ),
        "/" => (
            "200 OK",
            "text/html; charset=utf-8",
            "<title>Home page title that is long enough</title><h1>Home</h1><a href=\"/about\">About</a><a href=\"/private\">Private</a>".to_string(),
        ),
        "/about" | "/listed" => (
            "200 OK",
            "text/html; charset=utf-8",
            "<title>Inner page title that is long enough</title><h1>Inner</h1>".to_string(),
        ),
        _ => ("404 Not Found", "text/plain", "missing".to_string()),
    };
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

#[test]
fn every_html_mode_request_names_the_running_release_and_its_site() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    {
        let base = base.clone();
        let seen = seen.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let base = base.clone();
                let seen = seen.clone();
                thread::spawn(move || serve(stream, base, seen));
            }
        });
    }

    let result = crawl_site_with(
        &format!("{base}/"),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        CrawlOptions {
            sitemaps: true,
            rendering: RenderMode::Html,
            browser_path: None,
            ..CrawlOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );

    let seen = seen.lock().unwrap();
    // The robots group for CalmToad, not the wildcard that disallows everything, was applied.
    for path in ["/robots.txt", "/sitemap.xml", "/", "/about", "/listed"] {
        assert!(
            seen.contains_key(path),
            "expected a request for {path}; saw {:?} ({})",
            seen.keys().collect::<Vec<_>>(),
            result.message
        );
    }
    assert!(
        !seen.contains_key("/private"),
        "the CalmToad robots group disallows /private"
    );

    let product = format!("CalmToad/{}", env!("CARGO_PKG_VERSION"));
    for (path, agents) in seen.iter() {
        for agent in agents {
            assert!(
                agent.starts_with(&product),
                "{path} was requested as {agent:?}, not {product}"
            );
            assert!(
                agent.contains("+https://calmtoad.com"),
                "{path} was requested as {agent:?} without the calmtoad.com contact URL"
            );
        }
    }
}
