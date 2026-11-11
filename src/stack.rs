//! A large stack for modules that nest deeply.
//!
//! Pkl has no nesting limit of its own; it fails only when the JVM stack runs
//! out, which takes tens of thousands of nested expressions. pklr recurses
//! natively, so a caller's stack (2 MiB for a spawned thread) overflows far
//! sooner, and a fixed limit that is safe there rejects modules pkl accepts.
//!
//! An entry point first runs on the caller's stack with the small limits
//! below. Only if a limit is reached does it run again on a thread with a
//! large stack and limits sized for it, so the common module pays nothing
//! and a deep one evaluates or fails with an error, never an abort.

use std::cell::Cell;

/// Stack size of the evaluation thread. Address space is reserved up front but
/// only touched pages are committed.
const STACK_SIZE: usize = 256 << 20;

/// Stack kept in reserve for recursion between two depth checks.
const RESERVE: usize = 32 << 20;

/// Stack an evaluation may use on the caller's stack, of unknown size, before
/// it moves to the large one. A thread spawned by the standard library has
/// 2 MiB; a debug build spends tens of KiB per nesting level.
///
/// Unit tests read thread-local counters that an evaluation must update on the
/// test's own thread, so there it is unlimited; `tests/nesting.rs` covers the
/// move.
const CALLER_BUDGET: usize = if cfg!(test) { usize::MAX } else { 512 << 10 };

/// Nesting limit of the parser on the large stack.
pub(crate) const PARSER_DEPTH: usize = 50_000;

/// Nesting limit of the parser and lexer on a stack of unknown size, such as
/// a direct call to `parser::parse`.
pub(crate) const SMALL_STACK_PARSER_DEPTH: usize = 128;

/// Default evaluator recursion limit on the caller's stack.
pub(crate) const SMALL_STACK_EVAL_DEPTH: usize = 32;

/// Default evaluator recursion limit on the large stack.
pub(crate) const EVAL_DEPTH: usize = 50_000;

thread_local! {
    /// Lowest stack address that recursion may reach under [`run`]; zero
    /// outside it.
    static STACK_FLOOR: Cell<usize> = const { Cell::new(0) };
    /// Whether this thread is the large one started by [`run`].
    static LARGE: Cell<bool> = const { Cell::new(false) };
    /// Whether a limit was reached since the flag was last cleared.
    static LIMIT_HIT: Cell<bool> = const { Cell::new(false) };
    /// Whether an outer [`run`] is already running on this thread.
    static IN_RUN: Cell<bool> = const { Cell::new(false) };
}

/// Clears the state of the first run when it ends, even by a panic.
struct InRun;

impl Drop for InRun {
    fn drop(&mut self) {
        IN_RUN.with(|flag| flag.set(false));
        STACK_FLOOR.with(|floor| floor.set(0));
    }
}

/// The address of a local, which is near the top of the stack in use.
#[inline(never)]
fn stack_top() -> usize {
    let marker = 0u8;
    std::ptr::addr_of!(marker) as usize
}

/// Whether the current thread was started by [`run`].
pub(crate) fn on_large_stack() -> bool {
    LARGE.with(Cell::get)
}

/// Record that a nesting limit was reached, so that the entry point runs
/// again on the large stack. Cheap, and only called on the way to an error.
#[cold]
pub(crate) fn limit_hit() {
    LIMIT_HIT.with(|hit| hit.set(true));
}

/// Whether recursion under [`run`] has used the stack it may: the budget on
/// the caller's stack, or all of the large one but its reserve. Depth limits count levels, but the bytes a level
/// takes vary with the build and the kind of expression, so this is what
/// keeps a deep module an error instead of an abort.
#[inline]
pub(crate) fn exhausted() -> bool {
    let marker = 0u8;
    let floor = STACK_FLOOR.with(Cell::get);
    floor != 0 && (std::ptr::addr_of!(marker) as usize) < floor
}

/// An error if recursion has used the stack it may; see [`exhausted`].
pub(crate) fn check_depth() -> crate::Result<()> {
    if exhausted() {
        limit_hit();
        return Err(crate::Error::Eval(
            "maximum recursion depth exceeded".into(),
        ));
    }
    Ok(())
}

/// Run `f` with `false` on the current stack. If that reached a nesting
/// limit, run it again with `true` on a thread with a large stack, where the
/// limits are far higher. `f` must be able to start over. If the thread cannot
/// be created, the result of the first run stands.
pub(crate) fn run<R: Send>(mut f: impl FnMut(bool) -> R + Send) -> R {
    if on_large_stack() {
        return f(true);
    }
    // An entry point called by another one is part of the outer run.
    if IN_RUN.with(Cell::get) {
        return f(false);
    }
    LIMIT_HIT.with(|hit| hit.set(false));
    IN_RUN.with(|flag| flag.set(true));
    STACK_FLOOR.with(|floor| floor.set(stack_top().saturating_sub(CALLER_BUDGET).max(1)));
    let first = {
        let _in_run = InRun;
        f(false)
    };
    if !LIMIT_HIT.with(|hit| hit.replace(false)) {
        return first;
    }
    let spawned = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, || {
                LARGE.with(|large| large.set(true));
                STACK_FLOOR.with(|floor| {
                    floor.set(stack_top().saturating_sub(STACK_SIZE - RESERVE).max(1));
                });
                f(true)
            })
            .map(|handle| handle.join())
    });
    match spawned {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => first,
    }
}
