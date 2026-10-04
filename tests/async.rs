//! Tests for the optional `async` feature: the reqwest HTTP backend and the
//! async entry points.

mod common;

use pklr::EvalCapabilities;

fn multi_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn sync_evaluation_works_inside_and_outside_a_runtime() {
    let path = std::path::Path::new("tests/fixtures/base.pkl");
    let expected = pklr::eval_to_json(path).unwrap();

    let actual = current_thread_runtime().block_on(async { pklr::eval_to_json(path).unwrap() });

    assert_eq!(actual, expected);
}

#[test]
fn reqwest_batch_fetch_runs_concurrently_on_a_multi_thread_runtime() {
    const URLS: usize = 6;
    let (routes, _) = common::fan_out_routes(URLS - 1);
    let server = common::DelayedServer::start(&common::borrow_routes(&routes), common::DELAY);
    let urls: Vec<String> = routes
        .iter()
        .map(|(path, _)| format!("{}{path}", server.base))
        .chain([format!("{}/missing.pkl", server.base)])
        .collect();

    let (results, elapsed) = multi_thread_runtime().block_on(async move {
        tokio::spawn(async move {
            let mut capabilities =
                pklr::NativeCapabilities::with_reqwest_client(pklr::reqwest::Client::new());
            let started = std::time::Instant::now();
            let results = capabilities.fetch_text_many(&urls);
            (results, started.elapsed())
        })
        .await
        .unwrap()
    });

    assert_eq!(results.len(), routes.len() + 1);
    for ((_, body), result) in routes.iter().zip(&results) {
        assert_eq!(result.as_ref().unwrap(), body);
    }
    assert!(matches!(
        results.last().unwrap(),
        Err(pklr::Error::ImportNotFound(_))
    ));
    let sequential = common::DELAY * results.len() as u32;
    assert!(
        elapsed < sequential * 6 / 10,
        "took {elapsed:?}; sequential fetching takes {sequential:?}"
    );
}

#[test]
fn reqwest_fetch_works_outside_a_runtime() {
    let server = common::DelayedServer::start(&[("/a", "first")], std::time::Duration::ZERO);
    let mut capabilities =
        pklr::NativeCapabilities::with_reqwest_client(pklr::reqwest::Client::new());

    for _ in 0..2 {
        let body = capabilities
            .fetch_text(&format!("{}/a", server.base))
            .unwrap();
        assert_eq!(body, "first");
    }
}

#[test]
fn eval_async_on_a_current_thread_runtime() {
    let (routes, expected) = common::fan_out_routes(4);
    let server = common::DelayedServer::start(&common::borrow_routes(&routes), common::DELAY);
    let path = common::write_entry(
        "async_current_thread",
        "main.pkl",
        "import \"https://example.com/Main.pkl\"\nresult = Main.total\n",
    );
    let rewrite = format!("https://example.com/={}/", server.base);

    let json = current_thread_runtime().block_on(async {
        pklr::EvaluatorBuilder::new()
            .http_client(pklr::reqwest::Client::new())
            .http_rewrites([rewrite])
            .eval_to_json_async(&path)
            .await
            .unwrap()
    });

    assert_eq!(json["result"], expected);
    assert_eq!(server.requests(), 5);
}

#[test]
fn free_eval_to_json_async_matches_the_sync_result() {
    let path = std::path::Path::new("tests/fixtures/base.pkl");
    let expected = pklr::eval_to_json(path).unwrap();

    let actual = multi_thread_runtime()
        .block_on(pklr::eval_to_json_async(path))
        .unwrap();

    assert_eq!(actual, expected);
}

/// hk blocks on evaluation from inside `block_in_place` on a multi-threaded
/// runtime.
#[test]
fn eval_inside_block_in_place() {
    let (routes, expected) = common::fan_out_routes(3);
    let server =
        common::DelayedServer::start(&common::borrow_routes(&routes), std::time::Duration::ZERO);
    let path = common::write_entry(
        "async_block_in_place",
        "main.pkl",
        &format!("import \"{}/Main.pkl\"\nresult = Main.total\n", server.base),
    );

    let (from_async, from_sync) = multi_thread_runtime().block_on(async move {
        tokio::spawn(async move {
            let handle = tokio::runtime::Handle::current();
            tokio::task::block_in_place(|| {
                let from_async = handle
                    .block_on(
                        pklr::EvaluatorBuilder::new()
                            .http_client(pklr::reqwest::Client::new())
                            .eval_to_json_async(&path),
                    )
                    .unwrap();
                let from_sync = pklr::EvaluatorBuilder::new()
                    .http_client(pklr::reqwest::Client::new())
                    .eval_to_json(&path)
                    .unwrap();
                (from_async, from_sync)
            })
        })
        .await
        .unwrap()
    });

    assert_eq!(from_async["result"], expected);
    assert_eq!(from_sync, from_async);
}

#[test]
fn reqwest_batch_fetch_is_bounded() {
    let server = common::DelayedServer::start_with(
        |_| Some("value = 1\n".to_string()),
        std::time::Duration::from_millis(50),
    );
    let urls: Vec<String> = (0..20)
        .map(|index| format!("{}/{index}.pkl", server.base))
        .collect();

    let results = multi_thread_runtime().block_on(async move {
        tokio::spawn(async move {
            pklr::NativeCapabilities::with_reqwest_client(pklr::reqwest::Client::new())
                .fetch_text_many(&urls)
        })
        .await
        .unwrap()
    });

    assert_eq!(results.len(), 20);
    assert!(results.iter().all(|result| result.is_ok()));
    assert!(server.peak_in_flight() <= 8, "{}", server.peak_in_flight());
    assert!(server.peak_in_flight() > 1, "{}", server.peak_in_flight());
}

/// `eval_to_json_async` runs the evaluation under `spawn_blocking`, and the
/// reqwest backend then blocks on the caller's multi-threaded runtime with
/// `block_in_place` + `Handle::block_on` from that blocking thread. This
/// must not panic, for single fetches or for prefetch batches.
#[tokio::test(flavor = "multi_thread")]
async fn eval_async_with_reqwest_on_a_multi_thread_runtime() {
    let server = common::DelayedServer::start(
        &[("/A.pkl", "value = 1\n"), ("/B.pkl", "value = 2\n")],
        std::time::Duration::ZERO,
    );
    let path = common::write_entry(
        "async_multi_thread_reqwest",
        "main.pkl",
        &format!(
            "import \"{0}/A.pkl\"\nimport \"{0}/B.pkl\"\nresult = A.value + B.value\n",
            server.base
        ),
    );

    let json = pklr::EvaluatorBuilder::new()
        .http_client(pklr::reqwest::Client::new())
        .eval_to_json_async(&path)
        .await
        .unwrap();

    assert_eq!(json["result"], 3);
    assert_eq!(server.requests(), 2);
}

/// The current-thread counterpart of
/// `eval_async_with_reqwest_on_a_multi_thread_runtime`.
#[tokio::test(flavor = "current_thread")]
async fn eval_async_with_reqwest_on_a_current_thread_runtime() {
    let server = common::DelayedServer::start(
        &[("/A.pkl", "value = 1\n"), ("/B.pkl", "value = 2\n")],
        std::time::Duration::ZERO,
    );
    let path = common::write_entry(
        "async_current_thread_reqwest",
        "main.pkl",
        &format!(
            "import \"{0}/A.pkl\"\nimport \"{0}/B.pkl\"\nresult = A.value + B.value\n",
            server.base
        ),
    );

    let json = pklr::EvaluatorBuilder::new()
        .http_client(pklr::reqwest::Client::new())
        .eval_to_json_async(&path)
        .await
        .unwrap();

    assert_eq!(json["result"], 3);
    assert_eq!(server.requests(), 2);
}
