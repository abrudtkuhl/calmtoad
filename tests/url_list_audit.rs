//! URL-list scheduling and coverage against loopback fixtures.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use calmtoad::crawl::{CrawlOptions, Fetched, Limits, crawl_list_with, crawl_list_with_options};
use calmtoad::render::RenderMode;
use calmtoad::url_list::{Outcome, Seed};

// These tests each run a short-lived loopback HTTP server on an ephemeral port. Running many
// such fixtures simultaneously can make ureq's shared connection pool observe a server that is
// already being torn down, so keep the fixture scenarios serial while leaving each crawl's own
// request workers concurrent.
static URL_LIST_TEST_LOCK: Mutex<()> = Mutex::new(());

fn url_list_test_guard() -> MutexGuard<'static, ()> {
    URL_LIST_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone)]
struct Route {
    status: u16,
    content_type: &'static str,
    location: Option<String>,
    body: String,
    delay: std::time::Duration,
}

impl Route {
    fn html(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8",
            location: None,
            body: body.into(),
            delay: std::time::Duration::ZERO,
        }
    }

    fn redirect(location: impl Into<String>) -> Self {
        Self::redirect_status(302, location)
    }

    fn redirect_status(status: u16, location: impl Into<String>) -> Self {
        Self {
            status,
            content_type: "text/plain",
            location: Some(location.into()),
            body: String::new(),
            delay: std::time::Duration::ZERO,
        }
    }

    fn status(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain",
            location: None,
            body: format!("HTTP {status}"),
            delay: std::time::Duration::ZERO,
        }
    }

    fn robots(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "text/plain",
            location: None,
            body: body.into(),
            delay: std::time::Duration::ZERO,
        }
    }
}

struct Fixture {
    base: String,
    hits: Arc<Mutex<HashMap<String, usize>>>,
    shutdown: Arc<AtomicBool>,
}

impl Fixture {
    fn start(routes: HashMap<&'static str, Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        listener.set_nonblocking(true).expect("nonblocking fixture");
        let address = listener.local_addr().expect("fixture address");
        let base = format!("http://{address}");
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_hits = hits.clone();
        let thread_shutdown = shutdown.clone();
        thread::spawn(move || {
            while !thread_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let routes = routes.clone();
                        let hits = thread_hits.clone();
                        thread::spawn(move || serve(stream, &routes, &hits));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base,
            hits,
            shutdown,
        }
    }

    fn seeds(&self, paths: &[&str]) -> Vec<Seed> {
        paths
            .iter()
            .enumerate()
            .map(|(index, path)| Seed {
                source_row: index as u64 + 1,
                original: format!("{}{}", self.base, path),
                url: format!("{}{}", self.base, path),
            })
            .collect()
    }

    fn hits(&self, path: &str) -> usize {
        self.hits
            .lock()
            .unwrap()
            .get(path)
            .copied()
            .unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

fn serve(
    mut stream: TcpStream,
    routes: &HashMap<&'static str, Route>,
    hits: &Mutex<HashMap<String, usize>>,
) {
    let mut buffer = [0_u8; 8 * 1024];
    let read = stream.read(&mut buffer).unwrap_or(0);
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    let path = path.split('?').next().unwrap_or(path);
    *hits.lock().unwrap().entry(path.to_string()).or_default() += 1;
    let route = routes
        .get(path)
        .cloned()
        .unwrap_or_else(|| Route::status(404));
    if !route.delay.is_zero() {
        thread::sleep(route.delay);
    }
    let mut response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        route.status,
        if route.status == 200 {
            "OK"
        } else {
            "Response"
        },
        route.content_type,
        route.body.len()
    );
    if let Some(location) = &route.location {
        response.push_str(&format!("Location: {location}\r\n"));
    }
    response.push_str("\r\n");
    response.push_str(&route.body);
    let _ = stream.write_all(response.as_bytes());
}

fn html_page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><title>{title}</title>\
         <meta name=\"description\" content=\"A fixture description long enough for the content audit.\">\
         </head><body><h1>{title}</h1>{body}</body></html>"
    )
}

fn unchecked_count(checks: &[calmtoad::url_list::ListCheck]) -> usize {
    checks
        .iter()
        .filter(|check| check.outcome == Outcome::Unchecked)
        .count()
}

fn blocked_count(checks: &[calmtoad::url_list::ListCheck]) -> usize {
    checks
        .iter()
        .filter(|check| check.outcome == Outcome::Blocked)
        .count()
}

#[test]
fn list_mode_fetches_only_submitted_rows_and_keeps_link_evidence() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert("/robots.txt", Route::robots("User-agent: *\nAllow: /\n"));
    routes.insert(
        "/a",
        Route::html(html_page(
            "Listed A page with a long enough title",
            "<a href=\"/linked\">linked</a>",
        )),
    );
    routes.insert(
        "/b",
        Route::html(html_page("Listed B page with a long enough title", "")),
    );
    routes.insert(
        "/linked",
        Route::html(html_page("Discovered but not submitted", "")),
    );
    routes.insert(
        "/sitemap.xml",
        Route::html("<urlset><url><loc>/linked</loc></url></urlset>"),
    );
    let fixture = Fixture::start(routes);
    let seeds = fixture.seeds(&["/a", "/b"]);
    let options = CrawlOptions {
        sitemaps: true,
        check_links: true,
        ..CrawlOptions::default()
    };
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        options,
        seeds,
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );

    assert_eq!(result.list.len(), 2, "{}", result.message);
    assert!(
        result
            .list
            .iter()
            .all(|check| check.outcome == Outcome::Checked)
    );
    assert_eq!(fixture.hits("/linked"), 0);
    assert_eq!(fixture.hits("/sitemap.xml"), 0);
    assert_eq!(fixture.hits("/a"), 1);
    assert_eq!(fixture.hits("/b"), 1);
    let page_a = result
        .pages
        .iter()
        .find(|page| page.url == format!("{}/a", fixture.base))
        .expect("page a");
    assert!(
        page_a
            .link_evidence
            .iter()
            .any(|link| link.resolved.ends_with("/linked")
                && link.skip_reason == "Not scheduled in URL list mode")
    );
}

#[test]
fn allowed_redirect_to_a_default_port_default_origin_has_its_robots_policy_resolved() {
    let _guard = url_list_test_guard();
    let requested = Arc::new(Mutex::new(Vec::new()));
    let seen = requested.clone();
    let fetch = move |url: &str| {
        seen.lock().unwrap().push(url.to_string());
        if url.ends_with("/robots.txt") {
            return Fetched {
                status: 200,
                body: "User-agent: *\nAllow: /\n".into(),
                ..Fetched::default()
            };
        }
        if url.ends_with(":8123/a") {
            return Fetched {
                status: 302,
                location: "http://127.0.0.1/final".into(),
                ..Fetched::default()
            };
        }
        if url == "http://127.0.0.1/final" {
            return Fetched {
                status: 200,
                content_type: "text/html".into(),
                body: html_page("Default origin final page title", ""),
                ..Fetched::default()
            };
        }
        Fetched {
            status: 404,
            ..Fetched::default()
        }
    };
    let result = crawl_list_with_options(
        "http://127.0.0.1:8123/",
        Limits {
            max_pages: 4,
            max_depth: 3,
        },
        CrawlOptions::default(),
        vec![Seed {
            source_row: 1,
            original: "http://127.0.0.1:8123/a".into(),
            url: "http://127.0.0.1:8123/a".into(),
        }],
        fetch,
        None,
        |_| {},
    );
    let row = &result.list[0];
    assert_eq!(row.outcome, Outcome::Checked, "{}", row.reason);
    assert_eq!(row.status, 200);
    assert_eq!(row.final_url, "http://127.0.0.1/final");
    let requests = requested.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|url| url == "http://127.0.0.1/robots.txt"),
        "the allowed default-port origin policy was resolved: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .filter(|url| url.as_str() == "http://127.0.0.1/final")
            .count()
            == 1
    );
}

#[test]
fn redirect_chain_cap_reports_the_last_known_redirect_status() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert("/robots.txt", Route::robots("User-agent: *\nAllow: /\n"));
    routes.insert("/a", Route::redirect("/b"));
    routes.insert("/b", Route::redirect("/final"));
    routes.insert(
        "/final",
        Route::html(html_page("Final page with a long enough title", "")),
    );
    let fixture = Fixture::start(routes);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 2,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/a"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let row = &result.list[0];
    assert_eq!(row.outcome, Outcome::Unchecked);
    assert_eq!(row.status, 302);
    assert_eq!(row.final_url, format!("{}/final", fixture.base));
    assert_eq!(row.reason, "Page limit");
    assert_eq!(fixture.hits("/a"), 1);
    assert_eq!(fixture.hits("/b"), 1);
    assert_eq!(fixture.hits("/final"), 0);
}

#[test]
fn redirect_hop_limit_reports_unchecked_coverage_with_the_last_redirect_status() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert("/robots.txt", Route::robots("User-agent: *\nAllow: /\n"));
    for index in 1_u16..7 {
        routes.insert(
            Box::leak(format!("/{index}").into_boxed_str()),
            Route::redirect(format!("/{}", index + 1)),
        );
    }
    routes.insert(
        "/7",
        Route::html(html_page("Hop limit destination page title", "")),
    );
    let fixture = Fixture::start(routes);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/1"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let row = &result.list[0];
    assert_eq!(row.outcome, Outcome::Unchecked);
    assert_eq!(row.status, 302);
    assert_eq!(row.reason, "Redirect hop limit");
    assert_eq!(row.final_url, format!("{}/7", fixture.base));
    assert_eq!(fixture.hits("/6"), 1);
    assert_eq!(fixture.hits("/7"), 0);
}

#[test]
fn convergent_redirects_keep_each_sources_status_when_the_destination_is_over_cap() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert("/robots.txt", Route::robots("User-agent: *\nAllow: /\n"));
    routes.insert("/a", Route::redirect_status(301, "/final"));
    routes.insert("/b", Route::redirect_status(302, "/final"));
    routes.insert(
        "/final",
        Route::html(html_page("Converged destination page title", "")),
    );
    let fixture = Fixture::start(routes);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 2,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/a", "/b"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let row = |source_row: u64| {
        result
            .list
            .iter()
            .find(|check| check.source_row == source_row)
            .expect("row")
    };
    assert_eq!(row(1).outcome, Outcome::Unchecked);
    assert_eq!(row(2).outcome, Outcome::Unchecked);
    assert_eq!(row(1).reason, "Page limit");
    assert_eq!(row(2).reason, "Page limit");
    assert_eq!(row(1).status, 301);
    assert_eq!(row(2).status, 302);
    assert_eq!(row(1).final_url, format!("{}/final", fixture.base));
    assert_eq!(row(2).final_url, format!("{}/final", fixture.base));
    assert_eq!(fixture.hits("/a"), 1);
    assert_eq!(fixture.hits("/b"), 1);
    assert_eq!(fixture.hits("/final"), 0);
}

#[test]
fn convergent_redirects_keep_each_sources_status_when_the_destination_is_blocked() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert(
        "/robots.txt",
        Route::robots("User-agent: *\nDisallow: /blocked-final\n"),
    );
    routes.insert("/a", Route::redirect_status(301, "/blocked-final"));
    routes.insert("/b", Route::redirect_status(302, "/blocked-final"));
    routes.insert(
        "/blocked-final",
        Route::html(html_page("Blocked convergent destination title", "")),
    );
    let fixture = Fixture::start(routes);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/a", "/b"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let row = |source_row: u64| {
        result
            .list
            .iter()
            .find(|check| check.source_row == source_row)
            .expect("row")
    };
    assert_eq!(row(1).outcome, Outcome::Blocked);
    assert_eq!(row(2).outcome, Outcome::Blocked);
    assert_eq!(row(1).status, 301);
    assert_eq!(row(2).status, 302);
    assert!(row(1).reason.contains("disallows"));
    assert!(row(2).reason.contains("disallows"));
    assert_eq!(row(1).final_url, format!("{}/blocked-final", fixture.base));
    assert_eq!(row(2).final_url, format!("{}/blocked-final", fixture.base));
    assert_eq!(fixture.hits("/blocked-final"), 0);
}

#[test]
fn redirects_stay_in_scope_and_convergence_keeps_per_row_provenance() {
    let _guard = url_list_test_guard();
    let outside_listener = TcpListener::bind("127.0.0.1:0").expect("outside listener");
    outside_listener
        .set_nonblocking(true)
        .expect("outside nonblocking");
    let outside = AtomicUsize::new(0);
    let outside_hits = Arc::new(outside);
    let outside_url = format!("http://{}/secret", outside_listener.local_addr().unwrap());
    let thread_hits = outside_hits.clone();
    thread::spawn(move || {
        while let Ok((mut stream, _)) = outside_listener.accept() {
            thread_hits.fetch_add(1, Ordering::Relaxed);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
            );
        }
    });

    let mut routes = HashMap::new();
    routes.insert("/robots.txt", Route::robots("User-agent: *\nAllow: /\n"));
    routes.insert("/a", Route::redirect("/final"));
    routes.insert("/b", Route::redirect("/final"));
    routes.insert(
        "/final",
        Route::html(html_page("Converged destination page title", "")),
    );
    routes.insert("/outside", Route::redirect(outside_url.clone()));
    let fixture = Fixture::start(routes);
    let seeds = fixture.seeds(&["/a", "/b", "/outside"]);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        CrawlOptions::default(),
        seeds,
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );

    let row = |source_row: u64| {
        result
            .list
            .iter()
            .find(|check| check.source_row == source_row)
            .expect("row")
    };
    assert!(result.list[0].outcome == Outcome::Checked);
    assert!(result.list[1].outcome == Outcome::Checked);
    assert_eq!(row(1).final_url, format!("{}/final", fixture.base));
    assert_eq!(row(2).final_url, format!("{}/final", fixture.base));
    assert_eq!(row(1).status, 200);
    assert_eq!(row(2).status, 200);
    assert_eq!(row(3).outcome, Outcome::Blocked);
    assert_eq!(row(3).status, 302);
    assert!(row(3).reason.contains("scope"), "{}", row(3).reason);
    assert_eq!(outside_hits.load(Ordering::Relaxed), 0);
    assert_eq!(fixture.hits("/final"), 1);
}

#[test]
fn robots_blocks_rows_before_any_fetch_and_unavailable_policy_is_not_permission() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert(
        "/robots.txt",
        Route::robots("User-agent: *\nDisallow: /blocked\n"),
    );
    routes.insert(
        "/ok",
        Route::html(html_page("Allowed page with a long enough title", "")),
    );
    routes.insert(
        "/blocked",
        Route::html(html_page("Blocked page with a long enough title", "")),
    );
    let fixture = Fixture::start(routes);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/blocked", "/ok"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    assert_eq!(blocked_count(&result.list), 1);
    assert_eq!(fixture.hits("/blocked"), 0);
    assert_eq!(fixture.hits("/ok"), 1);
    assert!(result.list[0].reason.contains("disallows"));

    let mut unavailable = HashMap::new();
    unavailable.insert("/robots.txt", Route::status(500));
    unavailable.insert(
        "/x",
        Route::html(html_page("Never reached page with a long title", "")),
    );
    let fixture = Fixture::start(unavailable);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/x"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    assert_eq!(blocked_count(&result.list), 1);
    assert!(result.list[0].reason.contains("could not be read"));
    assert_eq!(fixture.hits("/x"), 0);
}

#[test]
fn robots_blocked_rows_consume_their_page_slot() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert(
        "/robots.txt",
        Route::robots("User-agent: *\nDisallow: /blocked\n"),
    );
    routes.insert(
        "/blocked",
        Route::html(html_page("Blocked page with a long enough title", "")),
    );
    routes.insert(
        "/ok",
        Route::html(html_page("Allowed page with a long enough title", "")),
    );
    let fixture = Fixture::start(routes);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 1,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/blocked", "/ok"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    assert_eq!(result.list[0].outcome, Outcome::Blocked);
    assert_eq!(result.list[1].outcome, Outcome::Unchecked);
    assert_eq!(result.list[1].reason, "Page limit");
    assert_eq!(fixture.hits("/blocked"), 0);
    assert_eq!(fixture.hits("/ok"), 0);
}

#[test]
fn blocked_row_in_the_middle_keeps_later_source_rows_out_of_the_budget() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert(
        "/robots.txt",
        Route::robots("User-agent: *\nDisallow: /blocked\n"),
    );
    routes.insert(
        "/one",
        Route::html(html_page("First page with a long enough title", "")),
    );
    routes.insert(
        "/blocked",
        Route::html(html_page("Blocked page with a long enough title", "")),
    );
    routes.insert(
        "/three",
        Route::html(html_page("Third page with a long enough title", "")),
    );
    let fixture = Fixture::start(routes);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 2,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/one", "/blocked", "/three"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    assert_eq!(result.list[0].outcome, Outcome::Checked);
    assert_eq!(result.list[1].outcome, Outcome::Blocked);
    assert_eq!(result.list[2].outcome, Outcome::Unchecked);
    assert_eq!(result.list[2].reason, "Page limit");
    assert_eq!(fixture.hits("/one"), 1);
    assert_eq!(fixture.hits("/blocked"), 0);
    assert_eq!(fixture.hits("/three"), 0);
}

#[test]
fn redirect_network_work_shares_the_page_budget_and_keeps_its_http_status() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert("/robots.txt", Route::robots("User-agent: *\nAllow: /\n"));
    routes.insert("/a", Route::redirect("/final"));
    routes.insert(
        "/final",
        Route::html(html_page("Final page with a long enough title", "")),
    );
    let fixture = Fixture::start(routes);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 1,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/a"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    let row = &result.list[0];
    assert_eq!(row.outcome, Outcome::Unchecked);
    assert_eq!(row.status, 302);
    assert_eq!(row.final_url, format!("{}/final", fixture.base));
    assert_eq!(row.reason, "Page limit");
    assert_eq!(fixture.hits("/a"), 1);
    assert_eq!(fixture.hits("/final"), 0);
}

#[test]
fn page_limit_is_deterministic_and_marks_the_tail_unchecked() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert("/robots.txt", Route::robots("User-agent: *\nAllow: /\n"));
    for path in ["/1", "/2", "/3", "/4"] {
        routes.insert(
            path,
            Route::html(html_page("A listed page with a long enough title", "")),
        );
    }
    let fixture = Fixture::start(routes);
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 2,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/1", "/2", "/3", "/4"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    assert_eq!(result.outcome, calmtoad::crawl::CrawlOutcome::Limited);
    assert_eq!(unchecked_count(&result.list), 2);
    assert_eq!(fixture.hits("/1"), 1);
    assert_eq!(fixture.hits("/2"), 1);
    assert_eq!(fixture.hits("/3"), 0);
    assert_eq!(fixture.hits("/4"), 0);
    assert_eq!(result.list[2].reason, "Page limit");
    assert_eq!(result.list[3].reason, "Page limit");
}

#[test]
fn stop_before_start_leaves_every_submitted_row_unchecked() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert("/robots.txt", Route::robots("User-agent: *\nAllow: /\n"));
    routes.insert(
        "/a",
        Route::html(html_page("A listed page with a long enough title", "")),
    );
    let fixture = Fixture::start(routes);
    let cancel = Arc::new(AtomicBool::new(true));
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/a"]),
        cancel,
        |_| {},
    );
    assert_eq!(result.outcome, calmtoad::crawl::CrawlOutcome::Stopped);
    assert_eq!(unchecked_count(&result.list), 1);
    assert_eq!(fixture.hits("/a"), 0);
}

#[test]
fn stop_after_a_completed_row_keeps_it_and_marks_the_rest_unchecked() {
    let _guard = url_list_test_guard();
    let mut routes = HashMap::new();
    routes.insert("/robots.txt", Route::robots("User-agent: *\nAllow: /\n"));
    routes.insert(
        "/a",
        Route::html(html_page("Completed row with a long enough title", "")),
    );
    routes.insert(
        "/slow",
        Route {
            status: 200,
            content_type: "text/html; charset=utf-8",
            location: None,
            body: html_page("Slow row with a long enough title", ""),
            delay: std::time::Duration::from_millis(750),
        },
    );
    let fixture = Fixture::start(routes);
    let cancel = Arc::new(AtomicBool::new(false));
    let event_cancel = cancel.clone();
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        CrawlOptions::default(),
        fixture.seeds(&["/a", "/slow"]),
        cancel,
        move |event| {
            if matches!(event, calmtoad::crawl::CrawlEvent::Page(_)) {
                event_cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(result.outcome, calmtoad::crawl::CrawlOutcome::Stopped);
    assert_eq!(result.list.len(), 2);
    assert_eq!(result.list[0].outcome, Outcome::Checked);
    assert_eq!(unchecked_count(&result.list), 1);
    assert!(result.list[1].reason.contains("Stopped"));
}

#[test]
fn browser_setup_failure_reports_each_submitted_row_unchecked() {
    let _guard = url_list_test_guard();
    let fixture = Fixture::start(HashMap::new());
    let options = CrawlOptions {
        rendering: RenderMode::Browser,
        browser_path: Some(std::path::PathBuf::from("/definitely/not/a/browser")),
        ..CrawlOptions::default()
    };
    let result = crawl_list_with(
        &format!("{}/", fixture.base),
        Limits {
            max_pages: 10,
            max_depth: 3,
        },
        options,
        fixture.seeds(&["/a", "/b"]),
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    assert!(result.setup_failure.is_some());
    assert_eq!(result.outcome, calmtoad::crawl::CrawlOutcome::Failed);
    assert_eq!(result.list.len(), 2);
    assert!(
        result
            .list
            .iter()
            .all(|check| check.outcome == Outcome::Unchecked)
    );
    assert!(
        result
            .list
            .iter()
            .all(|check| check.reason.contains("Browser"))
    );
}
