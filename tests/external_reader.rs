//! Reading modules and resources from an external reader process.
//!
//! This test binary is its own reader: run with `--reader` it serves `ext:`
//! modules and resources over Pkl's external reader protocol, using pklr's
//! message codec. Without it, it runs the tests, which start it again as the
//! reader. (`harness = false`, so the reader's stdout carries only protocol
//! messages.)

use std::io::{BufReader, BufWriter, Write};

use pklr::EvaluatorBuilder;
use pklr::external_reader::{
    Message, ModuleReaderSpec, ResourceReaderSpec, read_message, write_message,
};

fn main() {
    if std::env::args().any(|a| a == "--reader") {
        serve();
        return;
    }
    let tests: &[(&str, fn())] = &[
        ("amends_an_external_module", amends_an_external_module),
        ("reads_an_external_resource", reads_an_external_resource),
        ("reports_reader_errors", reports_reader_errors),
        (
            "reports_a_reader_that_fails_to_start",
            reports_a_reader_that_fails_to_start,
        ),
    ];
    for (name, test) in tests {
        test();
        println!("test {name} ... ok");
    }
}

fn module_source(uri: &str) -> Option<&'static str> {
    match uri {
        "ext:/Config.pkl" => {
            Some("import \"lib/Base.pkl\"\nname: String = Base.name\nport: Int = 80\n")
        }
        "ext:/lib/Base.pkl" => Some("name = \"from-reader\"\n"),
        _ => None,
    }
}

/// The reader side: answer requests until the evaluator asks to close.
fn serve() {
    let mut stdin = BufReader::new(std::io::stdin().lock());
    let mut stdout = BufWriter::new(std::io::stdout().lock());
    while let Some(message) = read_message(&mut stdin).unwrap() {
        let response = match message {
            Message::InitializeModuleReaderRequest { request_id, scheme } => {
                Message::InitializeModuleReaderResponse {
                    request_id,
                    spec: (scheme == "ext").then_some(ModuleReaderSpec {
                        scheme,
                        has_hierarchical_uris: true,
                        is_local: true,
                        is_globbable: false,
                    }),
                }
            }
            Message::InitializeResourceReaderRequest { request_id, scheme } => {
                Message::InitializeResourceReaderResponse {
                    request_id,
                    spec: (scheme == "ext").then_some(ResourceReaderSpec {
                        scheme,
                        has_hierarchical_uris: true,
                        is_globbable: false,
                    }),
                }
            }
            Message::ReadModuleRequest {
                request_id,
                evaluator_id,
                uri,
            } => match module_source(&uri) {
                Some(source) => Message::ReadModuleResponse {
                    request_id,
                    evaluator_id,
                    contents: Some(source.to_string()),
                    error: None,
                },
                None => Message::ReadModuleResponse {
                    request_id,
                    evaluator_id,
                    contents: None,
                    error: Some(format!("no module at {uri}")),
                },
            },
            Message::ReadResourceRequest {
                request_id,
                evaluator_id,
                uri,
            } => Message::ReadResourceResponse {
                request_id,
                evaluator_id,
                contents: Some(format!("resource {uri}").into_bytes()),
                error: None,
            },
            Message::CloseExternalProcess => return,
            _ => continue,
        };
        write_message(&mut stdout, &response).unwrap();
        stdout.flush().unwrap();
    }
}

fn reader() -> std::path::PathBuf {
    std::env::current_exe().unwrap()
}

fn main_module(name: &str, source: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pklr_external_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.pkl");
    std::fs::write(&path, source).unwrap();
    path
}

fn amends_an_external_module() {
    let main = main_module("amends", "amends \"ext:/Config.pkl\"\nport = 8080\n");
    let json = EvaluatorBuilder::new()
        .external_module_reader("ext", reader(), ["--reader"])
        .eval_to_json(&main)
        .unwrap();
    assert_eq!(
        json,
        serde_json::json!({"name": "from-reader", "port": 8080})
    );
}

fn reads_an_external_resource() {
    let main = main_module(
        "resource",
        "import \"ext:/lib/Base.pkl\"\nname = Base.name\ntext = read(\"ext:/data.txt\").text\n",
    );
    // One process serves both schemes.
    let json = EvaluatorBuilder::new()
        .external_module_reader("ext", reader(), ["--reader"])
        .external_resource_reader("ext", reader(), ["--reader"])
        .eval_to_json(&main)
        .unwrap();
    assert_eq!(
        json,
        serde_json::json!({"name": "from-reader", "text": "resource ext:/data.txt"})
    );
}

fn reports_reader_errors() {
    let main = main_module("errors", "import \"ext:/Nope.pkl\"\nx = Nope\n");
    let err = EvaluatorBuilder::new()
        .external_module_reader("ext", reader(), ["--reader"])
        .eval_to_json(&main)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no module at ext:/Nope.pkl"), "{err}");
}

fn reports_a_reader_that_fails_to_start() {
    let main = main_module("missing", "amends \"ext:/Config.pkl\"\n");
    let err = EvaluatorBuilder::new()
        .external_module_reader("ext", "/nonexistent/pklr-reader", Vec::<String>::new())
        .eval_to_json(&main)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Failed to start external reader"), "{err}");
}
