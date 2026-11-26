//! Pkl's [external reader] protocol.
//!
//! An external reader is a process that serves modules or resources under a
//! URI scheme, spoken to over its standard input and output with MessagePack
//! messages ([message passing API]). Both sides are here:
//!
//! - [`ExternalModuleReader`] and [`ExternalResourceReader`] start a reader
//!   process and adapt it to [`ModuleReader`] and [`ResourceReader`], which is
//!   what [`crate::EvaluatorBuilder::external_module_reader`] uses.
//! - [`Message`], [`read_message`] and [`write_message`] let a program
//!   implement a reader itself, to be run by `pkl` or by pklr.
//!
//! [external reader]: https://pkl-lang.org/main/current/language-reference/index.html#external-readers
//! [message passing API]: https://pkl-lang.org/main/current/bindings-specification/message-passing-api.html

use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rmpv::Value as Mp;

use crate::reader::{ModuleReader, ResourceReader};
use crate::{Error, Result};

/// What a reader declares about a module scheme it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleReaderSpec {
    /// The URI scheme.
    pub scheme: String,
    /// Whether URIs under the scheme are hierarchical (`scheme:/a/b.pkl`).
    pub has_hierarchical_uris: bool,
    /// Whether the modules are local to the system, which allows triple-dot
    /// imports between them.
    pub is_local: bool,
    /// Whether the scheme supports `import*`.
    pub is_globbable: bool,
}

/// What a reader declares about a resource scheme it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceReaderSpec {
    /// The URI scheme.
    pub scheme: String,
    /// Whether URIs under the scheme are hierarchical.
    pub has_hierarchical_uris: bool,
    /// Whether the scheme supports `read*`.
    pub is_globbable: bool,
}

/// An entry of a listing, for globbed imports and reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathElement {
    /// The entry's name.
    pub name: String,
    /// Whether the entry is a directory.
    pub is_directory: bool,
}

/// A message of the external reader protocol.
///
/// Requests come from the evaluator; responses and nothing else come from the
/// reader. Optional fields that are `None` are left out of the encoding.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `0x26`
    ReadResourceRequest {
        request_id: i64,
        evaluator_id: i64,
        uri: String,
    },
    /// `0x27`
    ReadResourceResponse {
        request_id: i64,
        evaluator_id: i64,
        contents: Option<Vec<u8>>,
        error: Option<String>,
    },
    /// `0x28`
    ReadModuleRequest {
        request_id: i64,
        evaluator_id: i64,
        uri: String,
    },
    /// `0x29`
    ReadModuleResponse {
        request_id: i64,
        evaluator_id: i64,
        contents: Option<String>,
        error: Option<String>,
    },
    /// `0x2a`
    ListResourcesRequest {
        request_id: i64,
        evaluator_id: i64,
        uri: String,
    },
    /// `0x2b`
    ListResourcesResponse {
        request_id: i64,
        evaluator_id: i64,
        path_elements: Option<Vec<PathElement>>,
        error: Option<String>,
    },
    /// `0x2c`
    ListModulesRequest {
        request_id: i64,
        evaluator_id: i64,
        uri: String,
    },
    /// `0x2d`
    ListModulesResponse {
        request_id: i64,
        evaluator_id: i64,
        path_elements: Option<Vec<PathElement>>,
        error: Option<String>,
    },
    /// `0x2e`
    InitializeModuleReaderRequest { request_id: i64, scheme: String },
    /// `0x2f`
    InitializeModuleReaderResponse {
        request_id: i64,
        spec: Option<ModuleReaderSpec>,
    },
    /// `0x30`
    InitializeResourceReaderRequest { request_id: i64, scheme: String },
    /// `0x31`
    InitializeResourceReaderResponse {
        request_id: i64,
        spec: Option<ResourceReaderSpec>,
    },
    /// `0x32`
    CloseExternalProcess,
    /// A message of a type this module does not know, by its code. Readers
    /// should ignore it; newer Pkl versions may send messages added later.
    Unknown(u8),
}

impl Message {
    fn code(&self) -> u8 {
        match self {
            Self::ReadResourceRequest { .. } => 0x26,
            Self::ReadResourceResponse { .. } => 0x27,
            Self::ReadModuleRequest { .. } => 0x28,
            Self::ReadModuleResponse { .. } => 0x29,
            Self::ListResourcesRequest { .. } => 0x2a,
            Self::ListResourcesResponse { .. } => 0x2b,
            Self::ListModulesRequest { .. } => 0x2c,
            Self::ListModulesResponse { .. } => 0x2d,
            Self::InitializeModuleReaderRequest { .. } => 0x2e,
            Self::InitializeModuleReaderResponse { .. } => 0x2f,
            Self::InitializeResourceReaderRequest { .. } => 0x30,
            Self::InitializeResourceReaderResponse { .. } => 0x31,
            Self::CloseExternalProcess => 0x32,
            Self::Unknown(code) => *code,
        }
    }

    fn body(&self) -> Vec<(&'static str, Mp)> {
        fn opt<T>(value: &Option<T>, f: impl Fn(&T) -> Mp) -> Option<Mp> {
            value.as_ref().map(f)
        }
        fn elements(elements: &[PathElement]) -> Mp {
            Mp::Array(
                elements
                    .iter()
                    .map(|element| {
                        map(vec![
                            ("name", Some(Mp::from(element.name.as_str()))),
                            ("isDirectory", Some(Mp::from(element.is_directory))),
                        ])
                    })
                    .collect(),
            )
        }
        let fields: Vec<(&'static str, Option<Mp>)> = match self {
            Self::ReadResourceRequest {
                request_id,
                evaluator_id,
                uri,
            }
            | Self::ReadModuleRequest {
                request_id,
                evaluator_id,
                uri,
            }
            | Self::ListResourcesRequest {
                request_id,
                evaluator_id,
                uri,
            }
            | Self::ListModulesRequest {
                request_id,
                evaluator_id,
                uri,
            } => vec![
                ("requestId", Some(Mp::from(*request_id))),
                ("evaluatorId", Some(Mp::from(*evaluator_id))),
                ("uri", Some(Mp::from(uri.as_str()))),
            ],
            Self::ReadResourceResponse {
                request_id,
                evaluator_id,
                contents,
                error,
            } => vec![
                ("requestId", Some(Mp::from(*request_id))),
                ("evaluatorId", Some(Mp::from(*evaluator_id))),
                ("contents", opt(contents, |c| Mp::Binary(c.clone()))),
                ("error", opt(error, |e| Mp::from(e.as_str()))),
            ],
            Self::ReadModuleResponse {
                request_id,
                evaluator_id,
                contents,
                error,
            } => vec![
                ("requestId", Some(Mp::from(*request_id))),
                ("evaluatorId", Some(Mp::from(*evaluator_id))),
                ("contents", opt(contents, |c| Mp::from(c.as_str()))),
                ("error", opt(error, |e| Mp::from(e.as_str()))),
            ],
            Self::ListResourcesResponse {
                request_id,
                evaluator_id,
                path_elements,
                error,
            }
            | Self::ListModulesResponse {
                request_id,
                evaluator_id,
                path_elements,
                error,
            } => vec![
                ("requestId", Some(Mp::from(*request_id))),
                ("evaluatorId", Some(Mp::from(*evaluator_id))),
                ("pathElements", opt(path_elements, |e| elements(e))),
                ("error", opt(error, |e| Mp::from(e.as_str()))),
            ],
            Self::InitializeModuleReaderRequest { request_id, scheme }
            | Self::InitializeResourceReaderRequest { request_id, scheme } => vec![
                ("requestId", Some(Mp::from(*request_id))),
                ("scheme", Some(Mp::from(scheme.as_str()))),
            ],
            Self::InitializeModuleReaderResponse { request_id, spec } => vec![
                ("requestId", Some(Mp::from(*request_id))),
                (
                    "spec",
                    opt(spec, |spec| {
                        map(vec![
                            ("scheme", Some(Mp::from(spec.scheme.as_str()))),
                            (
                                "hasHierarchicalUris",
                                Some(Mp::from(spec.has_hierarchical_uris)),
                            ),
                            ("isLocal", Some(Mp::from(spec.is_local))),
                            ("isGlobbable", Some(Mp::from(spec.is_globbable))),
                        ])
                    }),
                ),
            ],
            Self::InitializeResourceReaderResponse { request_id, spec } => vec![
                ("requestId", Some(Mp::from(*request_id))),
                (
                    "spec",
                    opt(spec, |spec| {
                        map(vec![
                            ("scheme", Some(Mp::from(spec.scheme.as_str()))),
                            (
                                "hasHierarchicalUris",
                                Some(Mp::from(spec.has_hierarchical_uris)),
                            ),
                            ("isGlobbable", Some(Mp::from(spec.is_globbable))),
                        ])
                    }),
                ),
            ],
            Self::CloseExternalProcess | Self::Unknown(_) => Vec::new(),
        };
        fields
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .collect()
    }

    fn decode(code: u8, body: &[(Mp, Mp)]) -> io::Result<Self> {
        let get = |key: &str| {
            body.iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .map(|(_, v)| v)
                .filter(|v| !v.is_nil())
        };
        let int = |key: &str| {
            get(key)
                .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
                .ok_or_else(|| invalid(format!("message {code:#x} has no integer `{key}`")))
        };
        let string = |key: &str| {
            get(key)
                .and_then(Mp::as_str)
                .map(str::to_string)
                .ok_or_else(|| invalid(format!("message {code:#x} has no string `{key}`")))
        };
        // A field that is present must have its type: a reader's error sent
        // as binary must not read as no error at all.
        let opt_string = |key: &str| -> io::Result<Option<String>> {
            match get(key) {
                None | Some(Mp::Nil) => Ok(None),
                Some(value) => value
                    .as_str()
                    .map(|s| Some(s.to_string()))
                    .ok_or_else(|| invalid(format!("message {code:#x} has no string `{key}`"))),
            }
        };
        let boolean = |value: &Mp, key: &str| {
            map_get(value, key)
                .and_then(Mp::as_bool)
                .ok_or_else(|| invalid(format!("reader spec has no boolean `{key}`")))
        };
        let spec_scheme = |value: &Mp| {
            map_get(value, "scheme")
                .and_then(Mp::as_str)
                .map(str::to_string)
                .ok_or_else(|| invalid("reader spec has no `scheme`".to_string()))
        };
        let path_elements = |key: &str| -> io::Result<Option<Vec<PathElement>>> {
            let Some(value) = get(key) else {
                return Ok(None);
            };
            let items = value
                .as_array()
                .ok_or_else(|| invalid(format!("`{key}` is not an array")))?;
            items
                .iter()
                .map(|item| {
                    Ok(PathElement {
                        name: map_get(item, "name")
                            .and_then(Mp::as_str)
                            .ok_or_else(|| invalid("path element has no `name`".to_string()))?
                            .to_string(),
                        is_directory: map_get(item, "isDirectory")
                            .and_then(Mp::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect::<io::Result<Vec<_>>>()
                .map(Some)
        };
        Ok(match code {
            0x26 => Self::ReadResourceRequest {
                request_id: int("requestId")?,
                evaluator_id: int("evaluatorId")?,
                uri: string("uri")?,
            },
            0x27 => Self::ReadResourceResponse {
                request_id: int("requestId")?,
                evaluator_id: int("evaluatorId")?,
                contents: match get("contents") {
                    Some(Mp::Binary(bytes)) => Some(bytes.clone()),
                    Some(Mp::String(s)) => Some(s.as_bytes().to_vec()),
                    Some(_) => return Err(invalid("`contents` is not binary".to_string())),
                    None => None,
                },
                error: opt_string("error")?,
            },
            0x28 => Self::ReadModuleRequest {
                request_id: int("requestId")?,
                evaluator_id: int("evaluatorId")?,
                uri: string("uri")?,
            },
            0x29 => Self::ReadModuleResponse {
                request_id: int("requestId")?,
                evaluator_id: int("evaluatorId")?,
                contents: opt_string("contents")?,
                error: opt_string("error")?,
            },
            0x2a => Self::ListResourcesRequest {
                request_id: int("requestId")?,
                evaluator_id: int("evaluatorId")?,
                uri: string("uri")?,
            },
            0x2b => Self::ListResourcesResponse {
                request_id: int("requestId")?,
                evaluator_id: int("evaluatorId")?,
                path_elements: path_elements("pathElements")?,
                error: opt_string("error")?,
            },
            0x2c => Self::ListModulesRequest {
                request_id: int("requestId")?,
                evaluator_id: int("evaluatorId")?,
                uri: string("uri")?,
            },
            0x2d => Self::ListModulesResponse {
                request_id: int("requestId")?,
                evaluator_id: int("evaluatorId")?,
                path_elements: path_elements("pathElements")?,
                error: opt_string("error")?,
            },
            0x2e => Self::InitializeModuleReaderRequest {
                request_id: int("requestId")?,
                scheme: string("scheme")?,
            },
            0x2f => Self::InitializeModuleReaderResponse {
                request_id: int("requestId")?,
                spec: match get("spec") {
                    Some(spec) => Some(ModuleReaderSpec {
                        scheme: spec_scheme(spec)?,
                        has_hierarchical_uris: boolean(spec, "hasHierarchicalUris")?,
                        is_local: boolean(spec, "isLocal")?,
                        is_globbable: boolean(spec, "isGlobbable")?,
                    }),
                    None => None,
                },
            },
            0x30 => Self::InitializeResourceReaderRequest {
                request_id: int("requestId")?,
                scheme: string("scheme")?,
            },
            0x31 => Self::InitializeResourceReaderResponse {
                request_id: int("requestId")?,
                spec: match get("spec") {
                    Some(spec) => Some(ResourceReaderSpec {
                        scheme: spec_scheme(spec)?,
                        has_hierarchical_uris: boolean(spec, "hasHierarchicalUris")?,
                        is_globbable: boolean(spec, "isGlobbable")?,
                    }),
                    None => None,
                },
            },
            0x32 => Self::CloseExternalProcess,
            code => Self::Unknown(code),
        })
    }

    /// The `requestId` of a request or response.
    pub fn request_id(&self) -> Option<i64> {
        match self {
            Self::ReadResourceRequest { request_id, .. }
            | Self::ReadResourceResponse { request_id, .. }
            | Self::ReadModuleRequest { request_id, .. }
            | Self::ReadModuleResponse { request_id, .. }
            | Self::ListResourcesRequest { request_id, .. }
            | Self::ListResourcesResponse { request_id, .. }
            | Self::ListModulesRequest { request_id, .. }
            | Self::ListModulesResponse { request_id, .. }
            | Self::InitializeModuleReaderRequest { request_id, .. }
            | Self::InitializeModuleReaderResponse { request_id, .. }
            | Self::InitializeResourceReaderRequest { request_id, .. }
            | Self::InitializeResourceReaderResponse { request_id, .. } => Some(*request_id),
            Self::CloseExternalProcess | Self::Unknown(_) => None,
        }
    }
}

fn map(fields: Vec<(&'static str, Option<Mp>)>) -> Mp {
    Mp::Map(
        fields
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (Mp::from(key), value)))
            .collect(),
    )
}

fn map_get<'a>(value: &'a Mp, key: &str) -> Option<&'a Mp> {
    value
        .as_map()?
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
        .filter(|v| !v.is_nil())
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Read the next message from `reader`, or `None` at the end of the stream.
pub fn read_message(reader: &mut impl Read) -> io::Result<Option<Message>> {
    let value = match rmpv::decode::read_value(reader) {
        Ok(value) => value,
        Err(rmpv::decode::Error::InvalidMarkerRead(error))
            if error.kind() == io::ErrorKind::UnexpectedEof =>
        {
            return Ok(None);
        }
        Err(rmpv::decode::Error::InvalidMarkerRead(error))
        | Err(rmpv::decode::Error::InvalidDataRead(error)) => return Err(error),
        Err(error) => return Err(invalid(error.to_string())),
    };
    let Mp::Array(parts) = value else {
        return Err(invalid("a message is not an array".to_string()));
    };
    let code = parts
        .first()
        .and_then(Mp::as_u64)
        .and_then(|code| u8::try_from(code).ok())
        .ok_or_else(|| invalid("a message has no type code".to_string()))?;
    let body = match parts.get(1) {
        Some(Mp::Map(body)) => body.as_slice(),
        Some(Mp::Nil) | None => &[],
        Some(_) => return Err(invalid(format!("message {code:#x} has no map body"))),
    };
    Message::decode(code, body).map(Some)
}

/// Write `message` to `writer` and flush it.
pub fn write_message(writer: &mut impl Write, message: &Message) -> io::Result<()> {
    let body = Mp::Map(
        message
            .body()
            .into_iter()
            .map(|(key, value)| (Mp::from(key), value))
            .collect(),
    );
    let value = Mp::Array(vec![Mp::from(message.code()), body]);
    rmpv::encode::write_value(writer, &value).map_err(io::Error::other)?;
    writer.flush()
}

/// How long a reader process gets to exit after it is asked to close, as Pkl
/// allows.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// The `evaluatorId` pklr sends: one per process is enough, since each
/// evaluator starts its own reader processes.
const EVALUATOR_ID: i64 = 1;

/// An external reader process, started on its first request and asked to
/// close when dropped. Requests are sent one at a time.
pub struct ExternalReaderProcess {
    executable: PathBuf,
    arguments: Vec<String>,
    state: Mutex<ProcessState>,
}

enum ProcessState {
    NotStarted,
    Running {
        child: Child,
        stdin: BufWriter<ChildStdin>,
        stdout: BufReader<ChildStdout>,
        next_request_id: i64,
    },
    Failed(String),
}

impl std::fmt::Debug for ExternalReaderProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalReaderProcess")
            .field("executable", &self.executable)
            .field("arguments", &self.arguments)
            .finish_non_exhaustive()
    }
}

impl ExternalReaderProcess {
    /// A reader process that runs `executable` with `arguments`. Nothing
    /// starts until the first request.
    pub fn new(executable: impl Into<PathBuf>, arguments: Vec<String>) -> Self {
        Self {
            executable: executable.into(),
            arguments,
            state: Mutex::new(ProcessState::NotStarted),
        }
    }

    /// The executable and arguments this process runs.
    pub fn command(&self) -> (&PathBuf, &[String]) {
        (&self.executable, &self.arguments)
    }

    fn describe(&self) -> String {
        let mut out = self.executable.display().to_string();
        for argument in &self.arguments {
            out.push(' ');
            out.push_str(argument);
        }
        out
    }

    /// Send `request` (built from a fresh request id) and wait for the
    /// response with the same id.
    fn request(&self, request: impl FnOnce(i64) -> Message) -> Result<Message> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let ProcessState::NotStarted = *state {
            *state = match self.spawn() {
                Ok(running) => running,
                Err(error) => ProcessState::Failed(format!(
                    "Failed to start external reader `{}`: {error}",
                    self.describe()
                )),
            };
        }
        let ProcessState::Running {
            stdin,
            stdout,
            next_request_id,
            ..
        } = &mut *state
        else {
            let ProcessState::Failed(error) = &*state else {
                unreachable!("the process was started above")
            };
            return Err(Error::Eval(error.clone()));
        };
        let request_id = *next_request_id;
        *next_request_id += 1;
        let result = (|| {
            write_message(stdin, &request(request_id))?;
            loop {
                let Some(message) = read_message(stdout)? else {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the process exited",
                    ));
                };
                if message.request_id() == Some(request_id) {
                    return Ok(message);
                }
            }
        })();
        result.map_err(|error| {
            let message = format!("External reader `{}` failed: {error}", self.describe());
            // A broken stream cannot be resynchronized, so later requests
            // report the same failure instead of reading garbage.
            if let ProcessState::Running { mut child, .. } =
                std::mem::replace(&mut *state, ProcessState::Failed(message.clone()))
            {
                let _ = child.kill();
                let _ = child.wait();
            }
            Error::Eval(message)
        })
    }

    fn spawn(&self) -> io::Result<ProcessState> {
        let mut child = Command::new(&self.executable)
            .args(&self.arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = BufWriter::new(child.stdin.take().expect("stdin is piped"));
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        Ok(ProcessState::Running {
            child,
            stdin,
            stdout,
            next_request_id: 1,
        })
    }
}

impl Drop for ExternalReaderProcess {
    fn drop(&mut self) {
        let state = std::mem::replace(
            self.state.get_mut().unwrap_or_else(|e| e.into_inner()),
            ProcessState::NotStarted,
        );
        let ProcessState::Running {
            mut child,
            mut stdin,
            ..
        } = state
        else {
            return;
        };
        let _ = write_message(&mut stdin, &Message::CloseExternalProcess);
        drop(stdin);
        let deadline = Instant::now() + CLOSE_TIMEOUT;
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// A [`ModuleReader`] backed by an external reader process.
#[derive(Debug)]
pub struct ExternalModuleReader {
    process: Arc<ExternalReaderProcess>,
    scheme: String,
    initialized: OnceLock<std::result::Result<(), String>>,
}

impl ExternalModuleReader {
    /// Read modules under `scheme` from `process`.
    pub fn new(process: Arc<ExternalReaderProcess>, scheme: &str) -> Self {
        Self {
            process,
            scheme: scheme.to_string(),
            initialized: OnceLock::new(),
        }
    }

    fn initialize(&self) -> Result<()> {
        self.initialized
            .get_or_init(|| {
                let response = self
                    .process
                    .request(|request_id| Message::InitializeModuleReaderRequest {
                        request_id,
                        scheme: self.scheme.clone(),
                    })
                    .map_err(|e| e.to_string())?;
                let describe = self.process.describe();
                match response {
                    Message::InitializeModuleReaderResponse { spec: Some(spec), .. } => {
                        if spec.has_hierarchical_uris {
                            Ok(())
                        } else {
                            Err(format!(
                                "External reader `{describe}` serves `{}:` modules by non-hierarchical URIs, which pklr does not support.",
                                self.scheme
                            ))
                        }
                    }
                    Message::InitializeModuleReaderResponse { spec: None, .. } => Err(format!(
                        "External reader `{describe}` does not read `{}:` modules.",
                        self.scheme
                    )),
                    other => Err(format!(
                        "External reader `{describe}` answered a module reader initialization with {other:?}."
                    )),
                }
            })
            .clone()
            .map_err(Error::Eval)
    }
}

impl ModuleReader for ExternalModuleReader {
    fn read_module(&self, uri: &str) -> Result<Option<String>> {
        self.initialize()?;
        let response = self
            .process
            .request(|request_id| Message::ReadModuleRequest {
                request_id,
                evaluator_id: EVALUATOR_ID,
                uri: uri.to_string(),
            })?;
        match response {
            Message::ReadModuleResponse {
                error: Some(error), ..
            } => Err(Error::Eval(format!("Cannot read module `{uri}`: {error}"))),
            Message::ReadModuleResponse { contents, .. } => Ok(Some(contents.unwrap_or_default())),
            other => Err(Error::Eval(format!(
                "External reader `{}` answered a module read with {other:?}.",
                self.process.describe()
            ))),
        }
    }

    fn external_process(&self) -> Option<Arc<ExternalReaderProcess>> {
        Some(Arc::clone(&self.process))
    }
}

/// A [`ResourceReader`] backed by an external reader process.
#[derive(Debug)]
pub struct ExternalResourceReader {
    process: Arc<ExternalReaderProcess>,
    scheme: String,
    initialized: OnceLock<std::result::Result<(), String>>,
}

impl ExternalResourceReader {
    /// Read resources under `scheme` from `process`.
    pub fn new(process: Arc<ExternalReaderProcess>, scheme: &str) -> Self {
        Self {
            process,
            scheme: scheme.to_string(),
            initialized: OnceLock::new(),
        }
    }

    fn initialize(&self) -> Result<()> {
        self.initialized
            .get_or_init(|| {
                let response = self
                    .process
                    .request(|request_id| Message::InitializeResourceReaderRequest {
                        request_id,
                        scheme: self.scheme.clone(),
                    })
                    .map_err(|e| e.to_string())?;
                let describe = self.process.describe();
                match response {
                    Message::InitializeResourceReaderResponse { spec: Some(_), .. } => Ok(()),
                    Message::InitializeResourceReaderResponse { spec: None, .. } => Err(format!(
                        "External reader `{describe}` does not read `{}:` resources.",
                        self.scheme
                    )),
                    other => Err(format!(
                        "External reader `{describe}` answered a resource reader initialization with {other:?}."
                    )),
                }
            })
            .clone()
            .map_err(Error::Eval)
    }
}

impl ResourceReader for ExternalResourceReader {
    fn read_resource(&self, uri: &str) -> Result<Option<Vec<u8>>> {
        self.initialize()?;
        let response = self
            .process
            .request(|request_id| Message::ReadResourceRequest {
                request_id,
                evaluator_id: EVALUATOR_ID,
                uri: uri.to_string(),
            })?;
        match response {
            Message::ReadResourceResponse {
                error: Some(error), ..
            } => Err(Error::Eval(format!(
                "Cannot read resource `{uri}`: {error}"
            ))),
            Message::ReadResourceResponse { contents, .. } => {
                Ok(Some(contents.unwrap_or_default()))
            }
            other => Err(Error::Eval(format!(
                "External reader `{}` answered a resource read with {other:?}.",
                self.process.describe()
            ))),
        }
    }

    fn external_process(&self) -> Option<Arc<ExternalReaderProcess>> {
        Some(Arc::clone(&self.process))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: Message) {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &message).unwrap();
        let mut cursor = io::Cursor::new(bytes);
        assert_eq!(read_message(&mut cursor).unwrap(), Some(message));
        assert_eq!(read_message(&mut cursor).unwrap(), None);
    }

    #[test]
    fn messages_round_trip() {
        round_trip(Message::InitializeModuleReaderRequest {
            request_id: -7_123_456_789,
            scheme: "hk".into(),
        });
        round_trip(Message::InitializeModuleReaderResponse {
            request_id: 3,
            spec: Some(ModuleReaderSpec {
                scheme: "hk".into(),
                has_hierarchical_uris: true,
                is_local: true,
                is_globbable: false,
            }),
        });
        round_trip(Message::InitializeResourceReaderResponse {
            request_id: 3,
            spec: None,
        });
        round_trip(Message::ReadModuleRequest {
            request_id: i64::MAX,
            evaluator_id: -1,
            uri: "hk:/Config.pkl".into(),
        });
        round_trip(Message::ReadModuleResponse {
            request_id: 1,
            evaluator_id: 2,
            contents: Some("foo = 1".into()),
            error: None,
        });
        round_trip(Message::ReadResourceResponse {
            request_id: 1,
            evaluator_id: 2,
            contents: None,
            error: Some("nope".into()),
        });
        round_trip(Message::ListModulesResponse {
            request_id: 1,
            evaluator_id: 2,
            path_elements: Some(vec![PathElement {
                name: "a.pkl".into(),
                is_directory: false,
            }]),
            error: None,
        });
        round_trip(Message::CloseExternalProcess);
    }

    #[test]
    fn a_mistyped_error_is_not_read_as_no_error() {
        let message = Mp::Array(vec![
            Mp::from(0x29),
            Mp::Map(vec![
                (Mp::from("requestId"), Mp::from(1)),
                (Mp::from("evaluatorId"), Mp::from(2)),
                (Mp::from("error"), Mp::Binary(b"boom".to_vec())),
            ]),
        ]);
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, &message).unwrap();
        let error = read_message(&mut bytes.as_slice()).unwrap_err();
        assert!(error.to_string().contains("`error`"), "{error}");
    }

    #[test]
    fn absent_optional_fields_are_left_out() {
        let mut bytes = Vec::new();
        write_message(
            &mut bytes,
            &Message::ReadModuleResponse {
                request_id: 1,
                evaluator_id: 2,
                contents: None,
                error: Some("missing".into()),
            },
        )
        .unwrap();
        let value = rmpv::decode::read_value(&mut bytes.as_slice()).unwrap();
        let body = value.as_array().unwrap()[1].as_map().unwrap();
        let keys: Vec<_> = body.iter().map(|(k, _)| k.as_str().unwrap()).collect();
        assert_eq!(keys, ["requestId", "evaluatorId", "error"]);
    }

    #[test]
    fn unknown_messages_are_kept_by_code() {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(
            &mut bytes,
            &Mp::Array(vec![Mp::from(0x7f), Mp::Map(Vec::new())]),
        )
        .unwrap();
        assert_eq!(
            read_message(&mut bytes.as_slice()).unwrap(),
            Some(Message::Unknown(0x7f))
        );
    }
}
