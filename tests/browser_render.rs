//! Real-browser proof for Browser mode.
//!
//! These tests launch the locally installed Chrome/Chromium (nothing is downloaded) against a
//! loopback HTTP fixture, so they exercise the same interception, readiness and cleanup code the
//! application uses. When no supported browser is installed the test reports a skip instead of
//! failing, because Browser mode is optional.
//!
//! Run with `cargo test --test browser_render -- --nocapture` to see the skip notice.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use calmtoad::crawl::{
    CrawlOptions, CrawlOutcome, Limits, auditable, crawl_site_with, title_issue,
};
use calmtoad::indexability::{CaptureInfo, EvidenceStage, FindingCode, Label};
use calmtoad::render::{
    BrowserRenderer, Capture, RenderConfig, RenderMode, RenderState, SourceState, discover_browser,
    kill_owned_browsers,
};

const PLAIN: &str = "<!doctype html><html><head><title>Fixture page title that is long enough</title>\
<meta name=\"description\" content=\"A fixture description that is comfortably long enough for the meta check to pass.\">\
</head><body><h1 id=\"heading\">Server heading</h1>\
<script>setTimeout(function () { document.getElementById('heading').textContent = 'Hydrated heading'; \
var link = document.createElement('a'); link.href = '/from-js'; link.textContent = 'Script link'; \
document.body.appendChild(link); }, 250);</script></body></html>";

const ROBOTS: &str = "<!doctype html><html><head><title>Header robots fixture page</title></head>\
<body><h1>Header robots</h1></body></html>";

const META_REFRESH: &str = "<!doctype html><html><head><title>Meta refresh fixture page</title>\
<meta http-equiv=\"refresh\" content=\"0; url=/plain\"></head><body><h1>Before refresh</h1></body></html>";

const OUTSIDE: &str = "<!doctype html><html><head><title>Outside scope fixture page</title></head>\
<body><h1>Outside</h1><script>\
window.location.href = 'http://localhost:HOST/destination';</script></body></html>";

const HOME: &str = "<!doctype html><html><head><title>Home page title that is comfortably long</title>\
<meta name=\"description\" content=\"A home description that is comfortably long enough for the meta check to pass.\">\
</head><body><h1>Home</h1>\
<a href=\"/plain\">plain one</a><a href=\"/plain\">plain two</a><a href=\"/redirect\">redirect</a>\
<a href=\"/noindex-source\">source robots</a><a href=\"/noindex-removed\">removed robots</a>\
<a href=\"/canonical-change\">canonical</a><a href=\"/hang\">hang</a><a href=\"/outside\">outside</a>\
<a href=\"/nolength-noindex\">no-length source robots</a>\
<a href=\"/canonical-remove\">canonical removed</a><a href=\"/canonical-add\">canonical added</a>\
<a href=\"/canonical-malformed\">malformed canonical</a>\
<a href=\"/canonical-same\">unchanged canonical</a>\
<a href=\"/history-path\">history path</a><a href=\"/history-loop\">history loop</a>\
<a href=\"/history-fragment\">history fragment</a><a href=\"/history-delayed\">delayed history</a>\
<a href=\"/broken-script\">broken script</a><a href=\"/missing-script\">missing script</a>\
<a href=\"/favicon-missing\">missing favicon</a>\
</body></html>";

const NOINDEX_SOURCE: &str = "<!doctype html><html><head><title>Source robots fixture page title</title>\
<meta name=\"description\" content=\"A source robots description that is comfortably long enough for the meta check.\">\
<meta name=\"robots\" content=\"noindex\"></head><body><h1>Source robots</h1></body></html>";

const NOINDEX_REMOVED: &str = "<!doctype html><html><head><title>Removed robots fixture page title</title>\
<meta name=\"description\" content=\"A removed robots description that is comfortably long enough for the meta check.\">\
<meta name=\"robots\" content=\"noindex\"></head><body><h1>Removed robots</h1>\
<script>document.querySelector('meta[name=robots]').remove();</script></body></html>";

const CANONICAL_CHANGE: &str = "<!doctype html><html><head><title>Canonical change fixture page title</title>\
<meta name=\"description\" content=\"A canonical change description that is comfortably long enough for the meta check.\">\
<link rel=\"canonical\" href=\"/canonical-original\"></head><body><h1>Canonical</h1>\
<script>document.querySelector('link[rel=canonical]').href = '/canonical-changed';</script>\
</body></html>";

const CANONICAL_REMOVE: &str = "<!doctype html><html><head><title>Canonical removal fixture page title</title>\
<meta name=\"description\" content=\"A canonical removal description that is comfortably long enough for the meta check.\">\
<link rel=\"canonical\" href=\"/canonical-source\"></head><body><h1>Canonical removal</h1>\
<script>document.querySelector('link[rel=canonical]').remove();</script></body></html>";

const CANONICAL_ADD: &str = "<!doctype html><html><head><title>Canonical addition fixture page title</title>\
<meta name=\"description\" content=\"A canonical addition description that is comfortably long enough for the meta check.\">\
</head><body><h1>Canonical addition</h1>\
<script>var link = document.createElement('link'); link.rel = 'canonical'; \
link.href = '/canonical-added'; document.head.appendChild(link);</script></body></html>";

const CANONICAL_MALFORMED: &str = "<!doctype html><html><head><title>Malformed canonical fixture page title</title>\
<meta name=\"description\" content=\"A malformed canonical description that is comfortably long enough for the meta check.\">\
<link rel=\"canonical\" href=\"mailto:x@example.com\"></head><body><h1>Malformed canonical</h1>\
<script>document.querySelector('link[rel=canonical]').href = '/canonical-fixed';</script>\
</body></html>";

const CANONICAL_SAME: &str = "<!doctype html><html><head><title>Unchanged canonical fixture page title</title>\
<meta name=\"description\" content=\"An unchanged canonical description that is comfortably long enough for the meta check.\">\
<link rel=\"canonical\" href=\"/canonical-same\"></head><body><h1>Unchanged canonical</h1>\
</body></html>";

const HISTORY_DELAYED: &str = "<!doctype html><html><head><title>Delayed history fixture page title</title>\
<meta name=\"description\" content=\"A delayed history description that is comfortably long enough for the meta check.\">\
</head><body><h1>Before transition</h1>\
<script>setTimeout(function () { history.replaceState(null, '', '/changed/path'); document.title = 'After transition'; var a = document.createElement('a'); a.href = 'child'; a.textContent = 'destination child'; document.body.appendChild(a); }, 250);</script>\
</body></html>";

const CHANGED_PATH: &str = "<!doctype html><html><head><title>Changed path fixture page title</title>\
<meta name=\"description\" content=\"A changed path description that is comfortably long enough for the meta check.\">\
</head><body><h1>Changed path document</h1><a href=\"child\">destination child</a></body></html>";

const BROKEN_SCRIPT: &str = "<!doctype html><html><head><title>Broken script shell fixture title</title>\
<meta name=\"description\" content=\"A broken script description that is comfortably long enough for the meta check.\">\
</head><body><h1>Application shell</h1><div id=\"app\"></div>\
<script src=\"/broken.js\"></script></body></html>";

const MISSING_SCRIPT: &str = "<!doctype html><html><head><title>Missing script shell fixture title</title>\
<meta name=\"description\" content=\"A missing script description that is comfortably long enough for the meta check.\">\
</head><body><h1>Application shell</h1><script src=\"/missing.js\"></script></body></html>";

const CROSS_SCRIPT: &str = "<!doctype html><html><head><title>Cross-origin script fixture title</title>\
<meta name=\"description\" content=\"A cross-origin script description that is comfortably long enough for the meta check.\">\
</head><body><h1>Cross-origin shell</h1><script src=\"http://localhost:PORT/broken.js\"></script></body></html>";

const WINDOW_OPEN: &str = "<!doctype html><html><head><title>Window open fixture page title</title>\
<meta name=\"description\" content=\"A window open description that is comfortably long enough for the meta check.\">\
</head><body><h1>Window opener</h1><script>window.open('/popup-target');</script></body></html>";

const FAVICON_MISSING: &str = "<!doctype html><html><head><title>Missing favicon fixture page title</title>\
<meta name=\"description\" content=\"A missing favicon description that is comfortably long enough for the meta check.\">\
<link rel=\"icon\" href=\"/missing.ico\"></head><body><h1>Favicon page</h1></body></html>";

const HISTORY_PATH: &str = "<!doctype html><html><head><title>History path fixture page title</title></head>\
<body><h1>Original path</h1><script>history.replaceState(null, '', '/history-destination');</script>\
</body></html>";

const HISTORY_FRAGMENT: &str = "<!doctype html><html><head><title>History fragment fixture page title</title></head>\
<body><h1>Fragment document</h1><script>history.replaceState(null, '', '/history-fragment#section');</script>\
</body></html>";

const HISTORY_QUERY: &str = "<!doctype html><html><head><title>History query fixture page title</title></head>\
<body><h1>Query document</h1><script>history.replaceState(null, '', '/history-query?changed=1');</script>\
</body></html>";

const HISTORY_LOOP: &str = "<!doctype html><html><head><title>History loop fixture page title</title></head>\
<body><h1>Loop one</h1><script>history.pushState(null, '', '/history-loop-2');</script></body></html>";

const HISTORY_LOOP_TWO: &str = "<!doctype html><html><head><title>History loop two fixture page title</title></head>\
<body><h1>Loop two</h1><script>history.pushState(null, '', '/history-loop');</script></body></html>";

const HISTORY_DESTINATION: &str = "<!doctype html><html><head><title>History destination fixture page title</title>\
<meta name=\"description\" content=\"A history destination description that is comfortably long enough for the meta check.\">\
<link rel=\"canonical\" href=\"history-relative-canonical\"></head><body><h1>Destination</h1>\
<a href=\"history-relative-target\">relative destination link</a></body></html>";

const UTF8: &str = "<!doctype html><html><head><title>UTF-8 fixture page title that is long enough</title></head>\
<body><h1>Unicode</h1><p>ééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééééé</p></body></html>";

const HANG_AFTER_LOAD: &str = "<!doctype html><html><head><title>Hung evaluation fixture page</title></head>\
<body><h1>Hung evaluation</h1>\
<script>setTimeout(function () { while (true) {} }, 0);</script></body></html>";

/// Browser work is expensive and these tests share one machine, so run them one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

struct Fixture {
    addr: SocketAddr,
    hits: Arc<Mutex<HashMap<String, usize>>>,
    endless_release: Arc<AtomicBool>,
}

impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        let addr = listener.local_addr().expect("fixture address");
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let shared = hits.clone();
        let endless_release = Arc::new(AtomicBool::new(false));
        let release = endless_release.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let hits = shared.clone();
                let release = release.clone();
                thread::spawn(move || serve(stream, hits, release));
            }
        });
        Self {
            addr,
            hits,
            endless_release,
        }
    }

    fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn hits(&self, path: &str) -> usize {
        hits_for(&self.hits, path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Release the held response even if an assertion fails.
        self.endless_release.store(true, Ordering::Relaxed);
    }
}

fn hits_for(hits: &Arc<Mutex<HashMap<String, usize>>>, path: &str) -> usize {
    hits.lock()
        .map(|hits| hits.get(path).copied().unwrap_or(0))
        .unwrap_or(0)
}

fn serve(
    stream: TcpStream,
    hits: Arc<Mutex<HashMap<String, usize>>>,
    endless_release: Arc<AtomicBool>,
) {
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
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    if let Ok(mut hits) = hits.lock() {
        *hits.entry(path.clone()).or_default() += 1;
    }
    let mut line = String::new();
    while reader
        .read_line(&mut line)
        .map(|read| read > 0)
        .unwrap_or(false)
    {
        if line == "\r\n" || line == "\n" {
            break;
        }
        line.clear();
    }

    let mut stream = stream;
    match path.as_str() {
        "/plain" => write_response(&mut stream, 200, "text/html; charset=utf-8", &[], PLAIN),
        "/notes.txt" => write_response(
            &mut stream,
            200,
            "text/plain; charset=utf-8",
            &[],
            "A plain text response with no HTML document.",
        ),
        "/robots-header" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[("X-Robots-Tag", "noindex"), ("X-Robots-Tag", "noindex")],
            ROBOTS,
        ),
        "/redirect" => write_response(
            &mut stream,
            302,
            "text/html; charset=utf-8",
            &[("Location", "/plain")],
            "",
        ),
        "/meta-refresh" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            META_REFRESH,
        ),
        "/outside" => {
            let body = OUTSIDE.replace("HOST", &stream.local_addr().unwrap().port().to_string());
            write_response(&mut stream, 200, "text/html; charset=utf-8", &[], &body);
        }
        "/destination" => write_response(&mut stream, 200, "text/html; charset=utf-8", &[], PLAIN),
        "/slow" => {
            thread::sleep(Duration::from_secs(8));
            write_response(&mut stream, 200, "text/html; charset=utf-8", &[], PLAIN)
        }
        "/hang" => {
            thread::sleep(Duration::from_secs(30));
            write_response(&mut stream, 200, "text/html; charset=utf-8", &[], PLAIN)
        }
        // A response that never finishes: headers arrive, then chunks keep coming. The
        // transfer cap must stop loading without waiting for the body or the deadline.
        "/endless" => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            );
            // Larger chunks reach Chrome's streaming notification threshold without
            // making this correctness test depend on small-chunk buffering or CI speed.
            let chunk = "a".repeat(32 * 1024);
            for _ in 0..16 {
                let frame = format!("{:x}\r\n{chunk}\r\n", chunk.len());
                if stream.write_all(frame.as_bytes()).is_err() || stream.flush().is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
            // Never finish until the test releases us. A disabled transfer cap must
            // produce TimedOut, not pass because this fixture happened to finish.
            while !endless_release.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(25));
            }
            let _ = stream.write_all(b"0\r\n\r\n");
        }
        "/home" => write_response(&mut stream, 200, "text/html; charset=utf-8", &[], HOME),
        "/noindex-source" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            NOINDEX_SOURCE,
        ),
        "/noindex-removed" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            NOINDEX_REMOVED,
        ),
        "/canonical-change" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            CANONICAL_CHANGE,
        ),
        "/nolength" => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.write_all(PLAIN.as_bytes());
            let _ = stream.flush();
        }
        "/nolength-noindex" => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n",
            );
            let _ = stream.write_all(NOINDEX_REMOVED.as_bytes());
            let _ = stream.flush();
        }
        "/canonical-remove" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            CANONICAL_REMOVE,
        ),
        "/canonical-add" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            CANONICAL_ADD,
        ),
        "/canonical-malformed" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            CANONICAL_MALFORMED,
        ),
        "/canonical-same" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            CANONICAL_SAME,
        ),
        "/history-delayed" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            HISTORY_DELAYED,
        ),
        "/changed/path" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            CHANGED_PATH,
        ),
        "/broken-script" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            BROKEN_SCRIPT,
        ),
        "/broken.js" => {
            // Advertise more than one byte so Chrome reports a transport failure mid-body.
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: 10000\r\nConnection: close\r\n\r\nx",
            );
            let _ = stream.flush();
        }
        "/missing-script" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            MISSING_SCRIPT,
        ),
        "/cross-script" => {
            // A script on a different origin (different host name, same loopback server).
            let port = stream
                .local_addr()
                .map(|address| address.port())
                .unwrap_or(1);
            let body = CROSS_SCRIPT.replace("PORT", &port.to_string());
            write_response(&mut stream, 200, "text/html; charset=utf-8", &[], &body);
        }
        "/window-open" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            WINDOW_OPEN,
        ),
        "/popup-target" => write_response(&mut stream, 200, "text/html; charset=utf-8", &[], PLAIN),
        "/missing.js" => write_response(&mut stream, 404, "text/javascript", &[], "not found"),
        "/favicon-missing" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            FAVICON_MISSING,
        ),
        "/missing.ico" => write_response(&mut stream, 404, "image/x-icon", &[], "not found"),
        "/history-path" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            HISTORY_PATH,
        ),
        "/history-fragment" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            HISTORY_FRAGMENT,
        ),
        "/history-query" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            HISTORY_QUERY,
        ),
        "/history-loop" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            HISTORY_LOOP,
        ),
        "/history-loop-2" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            HISTORY_LOOP_TWO,
        ),
        "/history-destination" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            HISTORY_DESTINATION,
        ),
        "/utf8" => write_response(&mut stream, 200, "text/html; charset=utf-8", &[], UTF8),
        "/hang-after-load" => write_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            &[],
            HANG_AFTER_LOAD,
        ),
        "/gzip-large" => {
            let body = "a".repeat(4096);
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(body.as_bytes()).expect("gzip fixture");
            let gz = encoder.finish().expect("gzip fixture");
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                gz.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&gz);
            let _ = stream.flush();
        }
        _ => write_response(&mut stream, 404, "text/plain", &[], "not found"),
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra: &[(&str, &str)],
    body: &str,
) {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        _ => "Not Found",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

/// Whether a missing browser is a failure instead of a skip.
///
/// CI sets `CALMTOAD_REQUIRE_BROWSER=1` so a runner without a browser cannot pass the suite by
/// silently skipping every real-browser test.
fn browser_required() -> bool {
    std::env::var("CALMTOAD_REQUIRE_BROWSER")
        .is_ok_and(|value| !matches!(value.trim(), "" | "0" | "false" | "no"))
}

fn skip_or_fail(reason: &str) {
    assert!(
        !browser_required(),
        "CALMTOAD_REQUIRE_BROWSER is set but no browser could run: {reason}"
    );
    eprintln!("skipping browser tests: {reason}");
}

fn renderer() -> Option<BrowserRenderer> {
    launch_renderer(RenderConfig {
        settle: Duration::from_millis(300),
        deadline: Duration::from_secs(12),
        ..RenderConfig::default()
    })
}

fn launch_renderer(config: RenderConfig) -> Option<BrowserRenderer> {
    let info = match discover_browser(None) {
        Ok(info) => info,
        Err(reason) => {
            skip_or_fail(&reason);
            return None;
        }
    };
    eprintln!("using {} ({})", info.label(), info.path.display());
    match BrowserRenderer::launch(config, Arc::new(AtomicBool::new(false))) {
        Ok(renderer) => Some(renderer),
        Err(reason) => {
            skip_or_fail(&reason);
            None
        }
    }
}

fn allow_base(base: &str) -> calmtoad::render::UrlFilter {
    let base = base.to_string();
    Arc::new(move |url: &str| url.starts_with(&base))
}

fn header<'a>(capture: &'a Capture, name: &str) -> Vec<&'a str> {
    capture
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .collect()
}

// Keep launch-switch and process-cleanup assertions active on every CI platform.
// Missing process inspection is a test failure, never evidence that Chrome exited.
fn process_commands() -> String {
    #[cfg(windows)]
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); Get-CimInstance Win32_Process -ErrorAction Stop | ForEach-Object { [Console]::WriteLine(([string]$_.ProcessId) + ' ' + $_.CommandLine) }",
        ])
        .output()
        .expect("Windows process inspection");
    #[cfg(not(windows))]
    let output = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
        .expect("Unix process inspection");
    assert!(
        output.status.success(),
        "process inspection failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 process commands")
}

#[test]
fn browser_mode_captures_scripted_dom_and_real_response_metadata() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let allowed = allow_base(&fixture.base());
    let url = format!("{}/plain", fixture.base());

    // The process really received well-formed switches, not a configured string that Chrome
    // silently ignores.
    let profile = renderer
        .profile_path()
        .map(|path| path.display().to_string())
        .expect("owned profile");
    let listing = process_commands();
    let pid = renderer
        .browser_pid()
        .expect("owned browser root")
        .to_string();
    let command = listing
        .lines()
        .find(|line| line.split_whitespace().next() == Some(pid.as_str()))
        .unwrap_or_else(|| panic!("no command for owned browser root {pid}"));
    assert!(command.contains(&profile), "root uses the owned profile");
    assert!(
        command.contains("--deny-permission-prompts"),
        "the switch keeps exactly one `--`: {command}"
    );
    assert!(
        !command.contains("----"),
        "no switch has a doubled prefix: {command}"
    );
    assert!(
        !command.contains("--no-sandbox"),
        "the sandbox stays enabled: {command}"
    );

    let capture = renderer.render(&url, allowed, Arc::new(AtomicBool::new(false)));

    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    assert_eq!(capture.status, 200);
    assert!(
        capture.content_type.contains("html"),
        "{}",
        capture.content_type
    );
    assert_eq!(capture.observed_url, url);
    assert_eq!(capture.requested_url, url);
    let source = capture.source_html.as_deref().expect("source body");
    assert!(source.contains("Server heading"), "source is pre-script");
    assert!(
        !source.contains(">Hydrated heading<"),
        "the source capture is the pre-script document: {source}"
    );
    let rendered = capture.rendered_html.as_deref().expect("rendered DOM");
    assert!(
        rendered.contains(">Hydrated heading<"),
        "the rendered DOM includes the delayed script change: {rendered}"
    );
    assert!(
        rendered.contains("/from-js"),
        "rendered links are discoverable"
    );
    assert!(capture.requests >= 1);
    assert!(capture.transfer_bytes > 0);
    assert!(capture.elapsed_ms < 12_000);
    assert!(
        !capture.browser.is_empty(),
        "the browser version is recorded"
    );
    assert_eq!(header(&capture, "content-type").len(), 1);

    renderer.shutdown();
}

#[test]
fn browser_mode_reports_a_dom_truncated_at_the_evidence_cap() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = launch_renderer(RenderConfig {
        settle: Duration::from_millis(300),
        deadline: Duration::from_secs(12),
        max_body_bytes: 256,
        ..RenderConfig::default()
    }) else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/plain", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );

    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    assert!(capture.rendered_truncated, "the DOM cap is reported");
    let rendered = capture.rendered_html.as_deref().expect("partial DOM");
    assert!(
        rendered.len() <= 257,
        "the retained DOM stops one byte past the cap: {}",
        rendered.len()
    );
    assert!(
        capture.source_html.is_none(),
        "an over-cap source body is not fetched just to truncate it"
    );
    assert!(
        capture.source_note.contains("cap"),
        "the missing source body is explained: {}",
        capture.source_note
    );

    renderer.shutdown();
}

#[test]
fn browser_mode_reports_a_resource_limit_as_truncated() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = launch_renderer(RenderConfig {
        settle: Duration::from_millis(300),
        deadline: Duration::from_secs(12),
        max_transfer_bytes: 1,
        ..RenderConfig::default()
    }) else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/plain", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );

    assert_eq!(capture.state, RenderState::Truncated, "{}", capture.reason);
    assert!(
        capture.reason.contains("resource limit"),
        "the cap is stated plainly: {}",
        capture.reason
    );
    assert!(capture.requests >= 1);

    renderer.shutdown();
}

#[test]
fn browser_mode_stops_a_never_finishing_response_at_the_transfer_cap() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = launch_renderer(RenderConfig {
        settle: Duration::from_millis(300),
        deadline: Duration::from_secs(12),
        max_transfer_bytes: 64 * 1024,
        ..RenderConfig::default()
    }) else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/endless", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(
        capture.state,
        RenderState::Truncated,
        "a body that never finishes is stopped by the transfer cap: {}",
        capture.reason
    );
    assert!(
        capture.reason.contains("resource limit"),
        "the cap is stated plainly: {}",
        capture.reason
    );
    assert_eq!(capture.source_state, SourceState::TooLarge);
    assert!(
        capture.source_html.is_none(),
        "the held source never finished"
    );
    assert!(
        capture.transfer_bytes >= 64 * 1024,
        "the streamed bytes are accounted while they arrive: {}",
        capture.transfer_bytes
    );
    renderer.shutdown();
}

#[test]
fn browser_mode_keeps_every_repeated_robots_header() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/robots-header", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    let directives = header(&capture, "x-robots-tag");
    assert_eq!(directives, vec!["noindex", "noindex"]);
    renderer.shutdown();
}

#[test]
fn browser_mode_stops_redirects_for_the_crawl_queue() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/redirect", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Redirected, "{}", capture.reason);
    assert_eq!(capture.status, 302);
    assert!(capture.location.ends_with("/plain"), "{}", capture.location);
    assert_eq!(
        fixture.hits("/plain"),
        0,
        "the browser never followed the redirect"
    );
    assert!(capture.rendered_html.is_none());
    renderer.shutdown();
}

#[test]
fn browser_mode_refuses_out_of_scope_documents_before_they_are_requested() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let allowed = allow_base(&fixture.base());

    let scripted = format!("{}/outside", fixture.base());
    let capture = renderer.render(&scripted, allowed.clone(), Arc::new(AtomicBool::new(false)));
    assert_eq!(capture.state, RenderState::Blocked, "{}", capture.reason);
    assert!(
        capture.navigation_to.contains("localhost"),
        "{}",
        capture.navigation_to
    );
    assert_eq!(
        fixture.hits("/destination"),
        0,
        "the refused destination was never requested"
    );
    assert!(
        capture.rendered_html.is_none(),
        "a refused navigation is not audited"
    );

    // The same host check applies before the browser is touched at all.
    let other_host = format!("http://localhost:{}/plain", fixture.addr.port());
    let capture = renderer.render(&other_host, allowed, Arc::new(AtomicBool::new(false)));
    assert_eq!(capture.state, RenderState::Blocked);
    assert_eq!(fixture.hits("/plain"), 0);
    renderer.shutdown();
}

#[test]
fn successful_browser_non_html_response_is_complete_context() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/notes.txt", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    assert!(
        capture
            .content_type
            .to_ascii_lowercase()
            .contains("text/plain"),
        "{}",
        capture.content_type
    );
    assert_eq!(capture.source_state, SourceState::NotApplicable);
    assert!(
        capture.rendered_html.is_none(),
        "a non-HTML response has no rendered document"
    );

    let info = CaptureInfo::browser(&capture);
    assert!(
        info.is_complete(),
        "a complete non-HTML observation is not an incomplete check"
    );
    assert_eq!(info.state_label(), "Browser · response");
    let assessment = calmtoad::indexability::assess(
        &url,
        capture.status,
        &capture.content_type,
        "",
        "",
        Default::default(),
        info,
    );
    assert_eq!(assessment.label, Label::NotAssessed);
    assert!(
        !assessment
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CaptureIncomplete),
        "no DOM is expected for non-HTML: {:?}",
        assessment.findings
    );
    renderer.shutdown();
}

#[test]
fn browser_mode_reports_a_meta_refresh_destination_without_following_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/meta-refresh", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    assert!(
        capture.navigation_to.ends_with("/plain"),
        "{}",
        capture.navigation_to
    );
    assert_eq!(
        fixture.hits("/plain"),
        0,
        "the destination belongs to the crawl queue"
    );
    let rendered = capture.rendered_html.as_deref().unwrap_or_default();
    assert!(rendered.contains("Before refresh"), "{rendered}");
    assert!(capture.reason.contains("queued"), "{}", capture.reason);
    renderer.shutdown();
}

#[test]
fn browser_mode_captures_a_source_body_without_content_length() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/nolength", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    assert_eq!(
        capture.source_state,
        calmtoad::render::SourceState::Captured,
        "a chunked body without Content-Length is still captured: {}",
        capture.source_note
    );
    let source = capture.source_html.as_deref().expect("source body");
    assert!(source.contains("Server heading"), "{source}");
    assert!(capture.source_note.is_empty(), "{}", capture.source_note);
    assert!(
        capture
            .rendered_html
            .as_deref()
            .unwrap_or_default()
            .contains(">Hydrated heading<")
    );
    renderer.shutdown();
}

#[test]
fn browser_mode_keeps_a_no_length_source_noindex_that_scripts_remove() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/nolength-noindex", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    let source = capture.source_html.as_deref().expect("source body");
    assert!(source.contains("name=\"robots\""), "{source}");
    assert!(
        !capture
            .rendered_html
            .as_deref()
            .unwrap_or_default()
            .contains("name=\"robots\"")
    );
    assert_eq!(
        capture.source_state,
        calmtoad::render::SourceState::Captured
    );
    renderer.shutdown();
}

#[test]
fn browser_mode_enforces_the_utf8_byte_cap() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = launch_renderer(RenderConfig {
        settle: Duration::from_millis(300),
        deadline: Duration::from_secs(12),
        max_body_bytes: 512,
        ..RenderConfig::default()
    }) else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/utf8", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    assert!(capture.rendered_truncated, "the UTF-8 cap is reported");
    let rendered = capture.rendered_html.as_deref().expect("partial DOM");
    // `String` is valid UTF-8 by construction; the cap is on encoded bytes, not UTF-16 units.
    assert!(
        rendered.len() <= 512,
        "the rendered DOM stops within the byte cap: {}",
        rendered.len()
    );
    assert!(
        !capture.source_state.is_complete(),
        "the oversized source is not presented as complete"
    );
    renderer.shutdown();
}

#[test]
fn browser_mode_does_not_expand_an_oversized_compressed_source() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = launch_renderer(RenderConfig {
        settle: Duration::from_millis(300),
        deadline: Duration::from_secs(12),
        max_body_bytes: 512,
        ..RenderConfig::default()
    }) else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/gzip-large", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    assert!(
        matches!(
            capture.source_state,
            SourceState::TooLarge | SourceState::Truncated
        ),
        "a compressed body that decodes past the cap is not complete: {:?} {}",
        capture.source_state,
        capture.source_note
    );
    if let Some(source) = capture.source_html.as_deref() {
        assert!(
            source.len() <= 512,
            "the bounded source stops at the cap: {}",
            source.len()
        );
    }
    renderer.shutdown();
}

#[test]
fn browser_mode_reports_history_path_changes_as_navigations() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let allowed = allow_base(&fixture.base());
    let cancel = Arc::new(AtomicBool::new(false));

    let url = format!("{}/history-path", fixture.base());
    let capture = renderer.render(&url, allowed.clone(), cancel.clone());
    assert_eq!(capture.state, RenderState::Navigated, "{}", capture.reason);
    assert!(
        capture.navigation_to.ends_with("/history-destination"),
        "{}",
        capture.navigation_to
    );
    assert!(
        capture.rendered_html.is_none(),
        "the destination's DOM is not audited as the original URL"
    );
    assert_eq!(
        capture.source_state,
        SourceState::Captured,
        "source evidence is still kept"
    );
    assert!(
        capture.observed_url.ends_with("/history-destination"),
        "{}",
        capture.observed_url
    );
    assert!(capture.reason.contains("queued"), "{}", capture.reason);
    assert_eq!(fixture.hits("/history-destination"), 0);

    // A query-only change is also a different document identity.
    let query_url = format!("{}/history-query", fixture.base());
    let query = renderer.render(&query_url, allowed.clone(), cancel.clone());
    assert_eq!(query.state, RenderState::Navigated, "{}", query.reason);
    assert!(
        query.navigation_to.ends_with("/history-query?changed=1"),
        "{}",
        query.navigation_to
    );
    assert!(query.rendered_html.is_none());

    // A fragment-only change keeps the same document and is audited normally.
    let fragment_url = format!("{}/history-fragment", fixture.base());
    let fragment = renderer.render(&fragment_url, allowed, cancel);
    assert_eq!(fragment.state, RenderState::Captured, "{}", fragment.reason);
    assert!(fragment.navigation_to.is_empty());
    assert!(
        fragment
            .rendered_html
            .as_deref()
            .unwrap_or_default()
            .contains("Fragment document")
    );
    renderer.shutdown();
}

#[test]
fn browser_mode_discards_a_delayed_history_destination_dom() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = launch_renderer(RenderConfig {
        settle: Duration::from_millis(800),
        deadline: Duration::from_secs(12),
        ..RenderConfig::default()
    }) else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/history-delayed", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Navigated, "{}", capture.reason);
    assert!(
        capture.navigation_to.ends_with("/changed/path"),
        "{}",
        capture.navigation_to
    );
    assert!(
        capture.rendered_html.is_none(),
        "a transition during settling must not leave the destination DOM attached"
    );
    assert_eq!(
        capture.source_state,
        SourceState::Captured,
        "source evidence for the requested URL is still kept"
    );
    assert_eq!(fixture.hits("/changed/path"), 0);
    assert_eq!(fixture.hits("/child"), 0);
    renderer.shutdown();
}

#[test]
fn browser_mode_marks_a_failed_script_as_incomplete() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let allowed = allow_base(&fixture.base());

    let broken_url = format!("{}/broken-script", fixture.base());
    let broken = renderer.render(
        &broken_url,
        allowed.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(broken.state, RenderState::Captured, "{}", broken.reason);
    assert_eq!(broken.source_state, SourceState::Captured);
    assert_eq!(
        broken.resource_failures.len(),
        1,
        "{:?}",
        broken.resource_failures
    );
    let failure = &broken.resource_failures[0];
    assert!(failure.url.ends_with("/broken.js"), "{failure:?}");
    assert_eq!(failure.resource_type, "Script");
    assert!(!failure.error.is_empty(), "{failure:?}");
    let info = CaptureInfo::browser(&broken);
    assert!(
        !info.is_complete(),
        "a failed script makes the capture incomplete"
    );
    assert!(
        info.state_label().contains("script failed"),
        "the capture label names the failure: {}",
        info.state_label()
    );

    let missing_url = format!("{}/missing-script", fixture.base());
    let missing = renderer.render(
        &missing_url,
        allowed.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(missing.state, RenderState::Captured, "{}", missing.reason);
    assert_eq!(
        missing.resource_failures.len(),
        1,
        "{:?}",
        missing.resource_failures
    );
    assert!(
        missing.resource_failures[0].error.contains("HTTP 404"),
        "{:?}",
        missing.resource_failures
    );
    assert!(!CaptureInfo::browser(&missing).is_complete());

    // A bundle on another origin is just as essential as a local one.
    let cross_url = format!("{}/cross-script", fixture.base());
    let cross = renderer.render(
        &cross_url,
        allowed.clone(),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(cross.state, RenderState::Captured, "{}", cross.reason);
    assert_eq!(
        cross.resource_failures.len(),
        1,
        "{:?}",
        cross.resource_failures
    );
    assert!(
        cross.resource_failures[0].url.contains("localhost"),
        "{:?}",
        cross.resource_failures
    );
    assert!(cross.resource_failures[0].url.ends_with("/broken.js"));
    assert!(!CaptureInfo::browser(&cross).is_complete());

    // A missing favicon is context, not an application script failure.
    let favicon_url = format!("{}/favicon-missing", fixture.base());
    let favicon = renderer.render(&favicon_url, allowed, Arc::new(AtomicBool::new(false)));
    assert_eq!(favicon.state, RenderState::Captured, "{}", favicon.reason);
    assert!(
        favicon.resource_failures.is_empty(),
        "{:?}",
        favicon.resource_failures
    );
    assert!(CaptureInfo::browser(&favicon).is_complete());
    renderer.shutdown();
}

#[cfg(unix)]
fn fake_browser(script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join("fake-chrome");
    std::fs::write(&path, script).expect("fake browser");
    let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("permissions");
    (dir, path)
}

#[cfg(unix)]
fn owned_count() -> usize {
    calmtoad::render::owned_browser_count()
}

#[cfg(unix)]
fn wait_for_owned_browser() {
    let deadline = Instant::now() + Duration::from_secs(5);
    while owned_count() == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        owned_count() > 0,
        "the spawned browser is registered before the handshake finishes"
    );
}

#[cfg(unix)]
fn no_process_references(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let listing = process_commands();
        let needle = path.display().to_string();
        let alive = listing.lines().any(|line| line.contains(&needle));
        if !alive {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "a process still references {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(unix)]
#[test]
fn stopping_during_browser_startup_kills_the_spawned_process() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let (_dir, script) = fake_browser(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'Fake Chrome 99.0'; exit 0; fi\nexec sleep 30\n",
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let config = RenderConfig {
        browser_path: Some(script.clone()),
        ..RenderConfig::default()
    };
    let started = Instant::now();
    let handle = thread::spawn(move || BrowserRenderer::launch(config, flag));
    wait_for_owned_browser();
    cancel.store(true, Ordering::Relaxed);
    let outcome = handle.join().expect("launch thread");
    assert!(
        outcome.is_err(),
        "cancelled startup does not return a renderer"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "Stop interrupts startup (took {:?})",
        started.elapsed()
    );
    assert_eq!(owned_count(), 0, "no owned browser is left");
    no_process_references(&script);
}

#[cfg(unix)]
#[test]
fn app_close_during_browser_startup_kills_the_registered_process() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let (_dir, script) = fake_browser(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'Fake Chrome 99.0'; exit 0; fi\nexec sleep 30\n",
    );
    let config = RenderConfig {
        browser_path: Some(script.clone()),
        ..RenderConfig::default()
    };
    let handle =
        thread::spawn(move || BrowserRenderer::launch(config, Arc::new(AtomicBool::new(false))));
    wait_for_owned_browser();
    // This is what app close and the final main sweep call.
    kill_owned_browsers();
    let outcome = handle.join().expect("launch thread");
    assert!(
        outcome.is_err(),
        "the killed handshake cannot produce a live renderer"
    );
    assert_eq!(owned_count(), 0);
    no_process_references(&script);
}

#[cfg(unix)]
#[test]
fn a_failed_browser_attach_leaves_no_process_behind() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let (_dir, script) = fake_browser(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'Fake Chrome 99.0'; exit 0; fi\necho 'DevTools listening on ws://127.0.0.1:1/devtools/browser/fake' >&2\nexec sleep 30\n",
    );
    let config = RenderConfig {
        browser_path: Some(script.clone()),
        ..RenderConfig::default()
    };
    let started = Instant::now();
    let outcome = BrowserRenderer::launch(config, Arc::new(AtomicBool::new(false)));
    assert!(outcome.is_err(), "a failed attach is an error");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "a failed attach is bounded (took {:?})",
        started.elapsed()
    );
    assert_eq!(owned_count(), 0);
    no_process_references(&script);
}

#[test]
fn window_open_does_not_fetch_its_destination() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/window-open", fixture.base());
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert_eq!(capture.state, RenderState::Captured, "{}", capture.reason);
    // The popup monitor is allowed to close a popup, but closing must not be the mechanism
    // that prevents the request.
    thread::sleep(Duration::from_millis(1_500));
    assert_eq!(
        fixture.hits("/popup-target"),
        0,
        "window.open must not fetch a destination the crawl did not choose"
    );
    renderer.shutdown();
}

#[test]
fn browser_mode_bounds_a_hung_dom_evaluation() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = launch_renderer(RenderConfig {
        settle: Duration::from_millis(100),
        deadline: Duration::from_secs(4),
        ..RenderConfig::default()
    }) else {
        return;
    };
    let fixture = Fixture::start();
    let url = format!("{}/hang-after-load", fixture.base());
    let started = Instant::now();
    let capture = renderer.render(
        &url,
        allow_base(&fixture.base()),
        Arc::new(AtomicBool::new(false)),
    );
    assert!(
        started.elapsed() < Duration::from_secs(9),
        "a hung evaluation is bounded (took {:?})",
        started.elapsed()
    );
    assert_ne!(
        capture.state,
        RenderState::Captured,
        "a hung evaluation is not a clean capture"
    );
    assert!(
        capture.reason.to_ascii_lowercase().contains("deadline"),
        "{}",
        capture.reason
    );
    renderer.shutdown();
}

#[test]
fn owned_browsers_can_be_killed_without_rust_destructors() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let pid = renderer.browser_pid().expect("owned browser process");
    let profile = renderer
        .profile_path()
        .map(ToOwned::to_owned)
        .expect("owned profile");
    let profile_arg = profile.display().to_string();
    kill_owned_browsers();
    assert!(
        !profile.exists(),
        "the owned profile is removed without a destructor"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let listing = process_commands();
        let stray = listing.lines().any(|line| line.contains(&profile_arg));
        if !stray {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned browser processes still reference {profile_arg} (root pid {pid})"
        );
        thread::sleep(Duration::from_millis(100));
    }
    // The renderer's own shutdown stays bounded even though its browser is already gone.
    renderer.shutdown();
}

#[test]
fn browser_mode_cancel_is_prompt_and_leaves_no_browser_behind() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(mut renderer) = renderer() else {
        return;
    };
    let fixture = Fixture::start();
    let profile = renderer
        .profile_path()
        .map(ToOwned::to_owned)
        .expect("owned profile");
    assert!(
        profile.exists(),
        "the profile exists while the browser runs"
    );

    let url = format!("{}/slow", fixture.base());
    let allowed = allow_base(&fixture.base());
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_flag = cancel.clone();
    let renderer_ref = &renderer;
    let started = Instant::now();
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        cancel_flag.store(true, Ordering::Relaxed);
    });
    let capture = renderer_ref.render(&url, allowed, cancel.clone());
    handle.join().expect("cancel thread");
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "cancel finished quickly (took {:?})",
        started.elapsed()
    );
    assert_eq!(capture.state, RenderState::Cancelled, "{}", capture.reason);
    assert!(capture.reason.contains("Stopped"), "{}", capture.reason);

    let profile_arg = profile.display().to_string();
    renderer.shutdown();
    assert!(
        !profile.exists(),
        "the application-owned profile is removed"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let listing = process_commands();
        if !listing.lines().any(|line| line.contains(&profile_arg)) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no browser process keeps the removed profile open"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn browser_mode_crawl_audits_rendered_documents_and_keeps_source_evidence() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let Some(info) = discover_browser(None).ok() else {
        skip_or_fail("no browser installed for the crawl-level test");
        return;
    };
    let fixture = Fixture::start();
    let seed = format!("{}/home", fixture.base());
    let options = CrawlOptions {
        sitemaps: false,
        rendering: RenderMode::Browser,
        browser_path: Some(info.path.clone()),
    };
    let outcome = crawl_site_with(
        &seed,
        Limits {
            max_pages: 30,
            max_depth: 2,
        },
        options,
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );

    let page = |path: &str| {
        let mut matches = outcome.pages.iter().filter(|page| page.url.ends_with(path));
        let first = matches.next().unwrap_or_else(|| {
            panic!(
                "{path} was not crawled: {:?}",
                outcome
                    .pages
                    .iter()
                    .map(|page| &page.url)
                    .collect::<Vec<_>>()
            )
        });
        assert!(
            matches.next().is_none(),
            "{path} was crawled more than once"
        );
        first
    };
    assert!(outcome.message.contains("pages on"), "{}", outcome.message);

    for page in &outcome.pages {
        assert_eq!(
            page.indexability.capture.mode,
            calmtoad::indexability::CaptureMode::Browser
        );
        assert!(
            page.indexability.capture.render.is_some(),
            "{} has browser provenance",
            page.url
        );
    }

    // The plain page is deduplicated and its audited H1 comes from the rendered DOM, not the
    // server response.
    let plain = page("/plain");
    assert_eq!(plain.status, 200);
    assert_eq!(plain.h1(), "Hydrated heading", "rendered DOM was audited");
    assert_eq!(
        plain.indexability.capture.render.as_ref().unwrap().state,
        RenderState::Captured
    );
    assert!(plain.indexability.capture.is_complete());
    assert!(
        plain.h1s.iter().all(|heading| heading != "Server heading"),
        "the pre-script heading is not audited as the document"
    );
    // A link the script added after load is discovered and crawled.
    assert_eq!(page("/from-js").status, 404);

    // A server redirect is handed to the crawl queue, which fetches the destination once.
    let redirect = page("/redirect");
    assert_eq!(redirect.status, 302);
    assert!(redirect.redirect_to.ends_with("/plain"));
    assert_eq!(
        redirect.indexability.capture.render.as_ref().unwrap().state,
        RenderState::Redirected
    );

    // A directive scripts removed is retained from the source capture.
    let removed = page("/noindex-removed");
    assert!(removed.noindex, "the source noindex still applies");
    let meta_stages: Vec<EvidenceStage> = removed
        .indexability
        .evidence
        .directives
        .iter()
        .filter(|directive| directive.agent.is_none())
        .map(|directive| directive.stage)
        .collect();
    assert_eq!(meta_stages, vec![EvidenceStage::Document]);
    assert!(
        removed
            .indexability
            .reason_text()
            .contains("Meta robots: noindex")
    );

    // A directive still present after rendering is recorded as rendered evidence.
    let source = page("/noindex-source");
    assert!(source.noindex);
    assert!(
        source
            .indexability
            .evidence
            .directives
            .iter()
            .any(|directive| {
                directive.agent.is_none() && directive.stage == EvidenceStage::Rendered
            })
    );

    // Scripts changing the canonical are audited from the rendered DOM and called out.
    let canonical = page("/canonical-change");
    assert!(
        canonical.canonical.ends_with("/canonical-changed"),
        "{}",
        canonical.canonical
    );
    assert!(
        canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalChanged),
        "{:?}",
        canonical.indexability.findings
    );
    assert!(
        !canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalConflict),
        "a cross-stage replacement is not a simultaneous conflict: {:?}",
        canonical.indexability.findings
    );

    // Canonical replacement, removal and addition are changes across captures, not conflicts.
    let removed_canonical = page("/canonical-remove");
    assert!(removed_canonical.canonical.is_empty());
    assert!(
        removed_canonical
            .indexability
            .evidence
            .canonicals
            .iter()
            .any(|declaration| declaration.stage == EvidenceStage::Document
                && declaration.raw == "/canonical-source"),
        "the source declaration stays as historical evidence"
    );
    assert!(
        removed_canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalChanged)
    );
    assert!(
        !removed_canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalConflict)
    );

    let added_canonical = page("/canonical-add");
    assert!(
        added_canonical.canonical.ends_with("/canonical-added"),
        "{}",
        added_canonical.canonical
    );
    assert!(
        added_canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalChanged)
    );
    assert!(
        !added_canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalConflict)
    );

    let fixed_canonical = page("/canonical-malformed");
    assert!(
        fixed_canonical.canonical.ends_with("/canonical-fixed"),
        "{}",
        fixed_canonical.canonical
    );
    assert!(
        fixed_canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalChanged)
    );
    assert!(
        !fixed_canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalConflict)
    );

    // An unchanged canonical is one observation per stage, not a script-made change.
    let same_canonical = page("/canonical-same");
    assert!(
        same_canonical.canonical.ends_with("/canonical-same"),
        "{}",
        same_canonical.canonical
    );
    assert_eq!(same_canonical.indexability.label, Label::NoBlockFound);
    assert!(
        !same_canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalChanged),
        "an unchanged canonical is not a change: {:?}",
        same_canonical.indexability.findings
    );
    assert!(
        !same_canonical
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CanonicalConflict)
    );

    // A transition during settling belongs to the destination, not the requested URL.
    let delayed = page("/history-delayed");
    let delayed_capture = delayed.indexability.capture.render.as_ref().unwrap();
    assert_eq!(
        delayed_capture.state,
        RenderState::Navigated,
        "{}",
        delayed_capture.reason
    );
    assert!(
        delayed_capture.navigation_to.ends_with("/changed/path"),
        "{}",
        delayed_capture.navigation_to
    );
    assert!(
        delayed.title.is_empty() && delayed.h1s.is_empty(),
        "the destination DOM is not audited as /history-delayed"
    );
    assert!(
        !delayed.links.iter().any(|link| link.ends_with("/child")),
        "the destination's link is not attributed to the original URL: {:?}",
        delayed.links
    );
    assert_eq!(fixture.hits("/child"), 0);
    assert_eq!(fixture.hits("/changed/path"), 1);
    assert_eq!(
        fixture.hits("/changed/child"),
        1,
        "the destination link resolves against /changed/path"
    );

    // A failed application script is incomplete, not a clean capture.
    let broken = page("/broken-script");
    assert_eq!(broken.status, 200);
    assert!(!auditable(broken));
    assert_eq!(broken.indexability.label, Label::NotAssessed);
    assert!(
        broken
            .indexability
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::CaptureIncomplete)
    );
    assert!(
        title_issue(broken, &calmtoad::crawl::Audit::default()).is_empty(),
        "an unhydrated shell does not become a title result"
    );
    let broken_render = broken.indexability.capture.render.as_ref().unwrap();
    assert_eq!(broken_render.resource_failures.len(), 1);
    assert!(
        broken_render.resource_failures[0]
            .url
            .ends_with("/broken.js"),
        "{:?}",
        broken_render.resource_failures
    );
    assert_eq!(broken_render.resource_failures[0].resource_type, "Script");

    // An HTTP-error script is equally incomplete.
    let missing_script = page("/missing-script");
    assert!(!auditable(missing_script));
    let missing_render = missing_script.indexability.capture.render.as_ref().unwrap();
    assert_eq!(missing_render.resource_failures.len(), 1);
    assert!(
        missing_render.resource_failures[0].error.contains("404"),
        "{:?}",
        missing_render.resource_failures
    );

    // A missing favicon is not an application failure.
    let favicon = page("/favicon-missing");
    assert!(auditable(favicon));
    assert!(
        favicon
            .indexability
            .capture
            .render
            .as_ref()
            .unwrap()
            .resource_failures
            .is_empty()
    );

    // A source noindex served without Content-Length is still evidence after scripts remove it.
    let no_length = page("/nolength-noindex");
    assert!(no_length.noindex, "the source noindex still applies");
    assert_ne!(no_length.indexability.label, Label::NoBlockFound);
    assert_eq!(
        no_length
            .indexability
            .capture
            .render
            .as_ref()
            .unwrap()
            .source_state,
        SourceState::Captured
    );
    assert!(
        no_length
            .indexability
            .evidence
            .directives
            .iter()
            .any(|directive| directive.stage == EvidenceStage::Document && directive.noindex)
    );

    // A same-document path change queues the destination and does not audit its DOM here.
    let history = page("/history-path");
    let history_capture = history.indexability.capture.render.as_ref().unwrap();
    assert_eq!(
        history_capture.state,
        RenderState::Navigated,
        "{}",
        history_capture.reason
    );
    assert!(
        history_capture
            .navigation_to
            .ends_with("/history-destination")
    );
    assert!(history.title.is_empty() && history.h1s.is_empty());
    assert!(!auditable(history));
    // The destination is crawled once, and its relative link and canonical resolve against it.
    let destination = page("/history-destination");
    assert_eq!(destination.status, 200);
    assert_eq!(destination.h1(), "Destination");
    assert!(
        destination
            .canonical
            .ends_with("/history-relative-canonical"),
        "{}",
        destination.canonical
    );
    assert_eq!(page("/history-relative-target").status, 404);
    assert_eq!(
        fixture.hits("/history-destination"),
        1,
        "the destination is admitted once through the crawl queue"
    );

    // A fragment-only change keeps the document and is audited normally.
    let fragment = page("/history-fragment");
    assert_eq!(
        fragment.indexability.capture.render.as_ref().unwrap().state,
        RenderState::Captured
    );
    assert_eq!(fragment.h1(), "Fragment document");

    // A loop between two history paths terminates at the already-seen destination.
    assert_eq!(fixture.hits("/history-loop"), 1);
    assert_eq!(fixture.hits("/history-loop-2"), 1);

    // A page that never finishes renders is incomplete, not silently clean.
    let hang = page("/hang");
    assert_eq!(
        hang.indexability.capture.render.as_ref().unwrap().state,
        RenderState::TimedOut
    );
    assert!(!hang.indexability.capture.is_complete());
    assert!(
        !auditable(hang),
        "an incomplete capture is excluded from completed content audits"
    );
    assert!(
        title_issue(hang, &calmtoad::crawl::Audit::default()).is_empty(),
        "incomplete source content does not become a title result"
    );
    assert_ne!(hang.indexability.label, Label::NoBlockFound);
    let incomplete = hang
        .indexability
        .findings
        .iter()
        .find(|finding| finding.code == FindingCode::CaptureIncomplete)
        .expect("an incomplete capture is reported");
    assert!(
        !hang
            .indexability
            .capture
            .render
            .as_ref()
            .unwrap()
            .reason
            .is_empty(),
        "the capture keeps the renderer's stop reason"
    );
    let reason = hang
        .indexability
        .capture
        .render
        .as_ref()
        .unwrap()
        .reason
        .trim()
        .trim_end_matches('.')
        .to_string();
    assert!(
        incomplete.guidance.contains(&reason),
        "the finding carries the stop reason: {}",
        incomplete.guidance
    );

    // A refused out-of-scope navigation is blocked before it is requested and the refused
    // destination is not crawled.
    let outside = page("/outside");
    let outside_capture = outside.indexability.capture.render.as_ref().unwrap();
    assert_eq!(
        outside_capture.state,
        RenderState::Blocked,
        "{}",
        outside_capture.reason
    );
    assert!(!outside.indexability.capture.is_complete());
    assert!(
        !outcome
            .pages
            .iter()
            .any(|page| page.url.contains("/destination")),
        "the refused destination belongs to another origin"
    );
    assert_eq!(
        fixture.hits("/destination"),
        0,
        "the out-of-scope destination was never requested"
    );

    // Browser pages run two at a time, so no page may claim a third concurrent render.
    assert!(outcome.pages.len() >= 8);
}

#[cfg(target_os = "macos")]
#[test]
fn discovery_accepts_an_application_bundle_without_a_version_probe() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().expect("temp dir");
    let app = dir.path().join("Slow Browser.app");
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos).expect("app contents");
    let executable = macos.join("Slow Browser");
    // A version probe would hang; discovery must not depend on one.
    std::fs::write(
        &executable,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then sleep 30; fi\nsleep 30\n",
    )
    .expect("fake browser");
    let mut permissions = std::fs::metadata(&executable)
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&executable, permissions).expect("permissions");

    let started = Instant::now();
    let info = discover_browser(Some(&app)).expect("a present application is usable");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(info.path, executable);
    assert_eq!(info.application.as_deref(), Some(app.as_path()));
    assert!(info.label().contains("Slow Browser"));
}
#[test]
fn an_explicit_browser_choice_is_not_silently_replaced() {
    let chosen = Path::new("/definitely/not/a/real/browser");
    let error = discover_browser(Some(chosen)).expect_err("an invalid choice is an error");
    assert!(
        error.contains("/definitely/not/a/real/browser"),
        "the message names the chosen executable: {error}"
    );
}

#[test]
fn browser_mode_stops_before_crawling_when_the_chosen_browser_cannot_run() {
    let outcome = crawl_site_with(
        "https://example.com/",
        Limits::default(),
        CrawlOptions {
            rendering: RenderMode::Browser,
            browser_path: Some(std::path::PathBuf::from("/definitely/not/a/real/browser")),
            ..CrawlOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    assert!(outcome.pages.is_empty());
    assert_eq!(outcome.message, "Browser could not start.");
    let detail = outcome.setup_failure.expect("a setup failure is typed");
    assert!(
        detail.contains("/definitely/not/a/real/browser")
            && detail.contains("not a browser application"),
        "{detail}"
    );
}

#[test]
fn a_stop_during_browser_startup_is_not_a_setup_failure() {
    let cancel = Arc::new(AtomicBool::new(true));
    let outcome = crawl_site_with(
        "https://example.com/",
        Limits::default(),
        CrawlOptions {
            rendering: RenderMode::Browser,
            ..CrawlOptions::default()
        },
        cancel,
        |_| {},
    );
    assert_eq!(outcome.outcome, CrawlOutcome::Stopped);
    assert!(outcome.setup_failure.is_none());
    assert!(outcome.message.contains("Stopped"));
}

#[cfg(unix)]
#[test]
fn a_stop_during_browser_preparation_returns_stopped_without_setup_failure() {
    // Browser lifecycle checks share the process-wide owned-browser registry.
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let (_dir, script) = fake_browser("#!/bin/sh\nsleep 30\n");
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_thread = cancel.clone();
    let handle = thread::spawn(move || {
        crawl_site_with(
            "https://example.com/",
            Limits::default(),
            CrawlOptions {
                rendering: RenderMode::Browser,
                browser_path: Some(script),
                ..CrawlOptions::default()
            },
            cancel_thread,
            |_| {},
        )
    });
    thread::sleep(Duration::from_millis(200));
    cancel.store(true, Ordering::Relaxed);
    let outcome = handle.join().expect("crawl thread");
    assert_eq!(outcome.outcome, CrawlOutcome::Stopped);
    assert!(
        outcome.setup_failure.is_none(),
        "a user stop is not a browser setup failure"
    );
    assert!(outcome.message.contains("Stopped"));
}

#[cfg(unix)]
#[test]
fn browser_launch_failure_is_typed_as_setup_not_a_domain_failure() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let (_dir, script) = fake_browser("#!/bin/sh\nexit 1\n");
    let outcome = crawl_site_with(
        "https://example.com/",
        Limits::default(),
        CrawlOptions {
            rendering: RenderMode::Browser,
            browser_path: Some(script.clone()),
            ..CrawlOptions::default()
        },
        Arc::new(AtomicBool::new(false)),
        |_| {},
    );
    assert!(outcome.pages.is_empty());
    assert_eq!(outcome.outcome, CrawlOutcome::Failed);
    assert_eq!(
        outcome.message, "Browser could not start.",
        "the run summary stays short"
    );
    let detail = outcome.setup_failure.expect("a setup failure is typed");
    assert!(
        !detail.trim().is_empty(),
        "the raw launch diagnostic is retained for Details"
    );
    assert!(
        detail.contains("debugging endpoint") || detail.contains("Could not start"),
        "the detail names the controlled launch failure: {detail}"
    );
}
