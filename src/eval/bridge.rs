//! Runs the synchronous evaluator on a worker thread for the async API.
//!
//! The evaluator core is synchronous. A blocking caller drives host IO by
//! blocking on each capability future in place. An async caller cannot do
//! that: the capability futures (for example reqwest requests) may need the
//! caller's runtime to make progress, and blocking a `current_thread` runtime
//! would deadlock. Instead the evaluator moves to a worker thread and sends
//! each capability call back to the async caller, which awaits it on its own
//! task and replies with the result.

use std::collections::VecDeque;
use std::future::poll_fn;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use crate::capabilities::{BoxFuture, EvalCapabilities};
use crate::{Error, Result};

/// A capability call sent from the worker to the async caller. It runs the
/// call against the caller's capabilities and sends the result back itself.
pub(crate) type IoRequest =
    Box<dyn for<'c> FnOnce(&'c mut dyn EvalCapabilities) -> BoxFuture<'c, ()> + Send>;

enum Message {
    Io(IoRequest),
    Done,
}

#[derive(Default)]
struct State {
    messages: VecDeque<Message>,
    waker: Option<Waker>,
    /// Set when the async caller stops listening (its future was dropped).
    closed: bool,
}

/// The worker's handle for sending capability calls to the async caller.
#[derive(Clone)]
pub(crate) struct Bridge {
    state: Arc<Mutex<State>>,
}

impl Bridge {
    fn send(&self, message: Message) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return false;
        }
        state.messages.push_back(message);
        if let Some(waker) = state.waker.take() {
            waker.wake();
        }
        true
    }

    /// Run a capability call on the async caller and wait for its result.
    pub(crate) fn call<T, F>(&self, call: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut dyn EvalCapabilities) -> BoxFuture<'c, Result<T>>
            + Send
            + 'static,
    {
        let (sender, receiver) = mpsc::sync_channel(1);
        let request: IoRequest = Box::new(move |capabilities| {
            let future = call(capabilities);
            Box::pin(async move {
                let _ = sender.send(future.await);
            })
        });
        if !self.send(Message::Io(request)) {
            return Err(cancelled());
        }
        receiver.recv().unwrap_or_else(|_| Err(cancelled()))
    }
}

fn cancelled() -> Error {
    Error::Eval("evaluation was cancelled".to_string())
}

/// Tells the async caller the worker is finished, even if it panicked.
struct DoneGuard(Bridge);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.send(Message::Done);
    }
}

/// Stops the worker's pending and future capability calls if the async
/// caller's future is dropped before the worker finishes.
struct CloseGuard(Arc<Mutex<State>>);

impl Drop for CloseGuard {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        // Dropping queued requests drops their reply senders, which wakes the
        // worker with an error.
        state.messages.clear();
    }
}

/// Stack size for the worker thread, matching a typical main thread so
/// deeply nested sources behave the same as under the blocking API.
const WORKER_STACK_SIZE: usize = 8 * 1024 * 1024;

/// Run `work` on a worker thread, serving its capability calls with
/// `capabilities` on the current task until it finishes.
pub(crate) async fn run<S, R>(
    subject: S,
    capabilities: &mut dyn EvalCapabilities,
    work: impl FnOnce(S, Bridge) -> (S, R) + Send + 'static,
) -> (S, R)
where
    S: Send + 'static,
    R: Send + 'static,
{
    let state = Arc::new(Mutex::new(State::default()));
    let _close = CloseGuard(state.clone());
    let bridge = Bridge {
        state: state.clone(),
    };
    let worker = std::thread::Builder::new()
        .name("pklr-eval".to_string())
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || {
            let _done = DoneGuard(bridge.clone());
            work(subject, bridge)
        })
        .expect("failed to spawn pklr evaluation thread");
    loop {
        let message = poll_fn(|cx| {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            match state.messages.pop_front() {
                Some(message) => Poll::Ready(message),
                None => {
                    state.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            }
        })
        .await;
        match message {
            Message::Io(request) => request(capabilities).await,
            Message::Done => break,
        }
    }
    match worker.join() {
        Ok(output) => output,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Stands in for the host capabilities while they are lent to the async
/// caller. The evaluator routes every capability call through the bridge
/// then, so these are never reached.
pub(crate) struct Detached;

fn detached<T>() -> BoxFuture<'static, Result<T>> {
    Box::pin(async {
        Err(Error::Unsupported(
            "evaluator capabilities are in use by a running evaluation".to_string(),
        ))
    })
}

impl EvalCapabilities for Detached {
    fn read_to_string<'a>(&'a mut self, _: &'a std::path::Path) -> BoxFuture<'a, Result<String>> {
        detached()
    }

    fn path_exists<'a>(&'a mut self, _: &'a std::path::Path) -> BoxFuture<'a, Result<bool>> {
        detached()
    }

    fn canonicalize<'a>(
        &'a mut self,
        _: &'a std::path::Path,
    ) -> BoxFuture<'a, Result<std::path::PathBuf>> {
        detached()
    }

    fn read_env<'a>(&'a mut self, _: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        detached()
    }

    fn fetch_text<'a>(&'a mut self, _: &'a str) -> BoxFuture<'a, Result<String>> {
        detached()
    }

    fn fetch_bytes<'a>(&'a mut self, _: &'a str) -> BoxFuture<'a, Result<Vec<u8>>> {
        detached()
    }

    fn temp_dir<'a>(&'a mut self, _: &'a str) -> BoxFuture<'a, Result<std::path::PathBuf>> {
        detached()
    }

    fn glob<'a>(
        &'a mut self,
        _: &'a std::path::Path,
        _: &'a str,
    ) -> BoxFuture<'a, Result<Vec<std::path::PathBuf>>> {
        detached()
    }
}
