//! Helpers shared by the HTTP tests.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A local HTTP server that answers every request after `delay`, each
/// connection on its own thread, so concurrent requests overlap.
pub struct DelayedServer {
    pub base: String,
    requests: Arc<AtomicUsize>,
    peak_in_flight: Arc<AtomicUsize>,
}

impl DelayedServer {
    /// Serve `routes` (path to body); any other path gets a 404.
    pub fn start(routes: &[(&str, &str)], delay: Duration) -> Self {
        let routes: HashMap<String, String> = routes
            .iter()
            .map(|(path, body)| (path.to_string(), body.to_string()))
            .collect();
        Self::start_with(move |path| routes.get(path).cloned(), delay)
    }

    /// Serve the body `respond` returns for each path, or a 404 for `None`.
    pub fn start_with(
        respond: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
        delay: Duration,
    ) -> Self {
        let routes = Arc::new(respond);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak_in_flight = Arc::new(AtomicUsize::new(0));
        let peak = peak_in_flight.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let routes = routes.clone();
                let counter = counter.clone();
                let in_flight = in_flight.clone();
                let peak = peak.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request_line = String::new();
                    if reader.read_line(&mut request_line).is_err() {
                        return;
                    }
                    let path = request_line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or("/")
                        .to_string();
                    loop {
                        let mut line = String::new();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) if line == "\r\n" => break,
                            Ok(_) => {}
                        }
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(delay);
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    let response = match routes(&path) {
                        Some(body) => format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        ),
                        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string(),
                    };
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        Self {
            base,
            requests,
            peak_in_flight,
        }
    }

    /// The most requests the server was handling at once.
    #[allow(dead_code)]
    pub fn peak_in_flight(&self) -> usize {
        self.peak_in_flight.load(Ordering::SeqCst)
    }

    /// The number of requests served so far.
    #[allow(dead_code)]
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

/// The delay each test server request takes.
pub const DELAY: Duration = Duration::from_millis(200);

/// A remote module that imports `count` sibling modules, each defining
/// `value = <index>`, and sums them. Returns the routes and the expected sum.
pub fn fan_out_routes(count: usize) -> (Vec<(String, String)>, i64) {
    let mut main = String::new();
    let mut sum = Vec::new();
    let mut routes = Vec::new();
    for index in 0..count {
        main.push_str(&format!("import \"Dep{index}.pkl\"\n"));
        sum.push(format!("Dep{index}.value"));
        routes.push((format!("/Dep{index}.pkl"), format!("value = {index}\n")));
    }
    main.push_str(&format!("total = {}\n", sum.join(" + ")));
    routes.push(("/Main.pkl".to_string(), main));
    (routes, (0..count as i64).sum())
}

/// Borrow owned routes for [`DelayedServer::start`].
pub fn borrow_routes(routes: &[(String, String)]) -> Vec<(&str, &str)> {
    routes
        .iter()
        .map(|(path, body)| (path.as_str(), body.as_str()))
        .collect()
}

/// Write `contents` to a fresh file named `name` in a per-test temp dir.
pub fn write_entry(test: &str, name: &str, contents: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pklr_{test}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

/// A server whose `/N.pkl` imports `N+1.pkl` forever, each module also
/// defining `value = N`.
#[allow(dead_code)]
pub fn endless_chain_server() -> DelayedServer {
    DelayedServer::start_with(
        |path| {
            let index: u64 = path.strip_prefix('/')?.strip_suffix(".pkl")?.parse().ok()?;
            Some(format!(
                "import \"{}.pkl\" as Next\nvalue = {index}\n",
                index + 1
            ))
        },
        Duration::ZERO,
    )
}
