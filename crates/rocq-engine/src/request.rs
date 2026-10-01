//! Request-scoped cancellation shared by synchronous Dune and PET work.

use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

const ACTIVE: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTED: u8 = 2;
const TIMED_OUT: u8 = 3;

/// One admitted MCP request's cancellation signal.
///
/// The async MCP layer owns cancellation; the synchronous engine observes a
/// clone while it waits for PET. Cancelling the signal never mutates proof
/// topology by itself. The PET transport terminates its current process epoch
/// before returning a cancellation error, after which MCP invalidates opaque
/// state IDs and retains the replayable checkpoint graph.
#[derive(Clone, Default)]
pub struct RequestCancellation {
    state: Arc<AtomicU8>,
}

impl RequestCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark this request cancelled. This method is lock-free and may be called
    /// from the async runtime while the request runs on a blocking worker. A
    /// cancellation that arrives after the operation's explicit commit point
    /// loses the race and cannot interrupt the committed transaction.
    pub fn cancel(&self) {
        let _ = self
            .state
            .compare_exchange(ACTIVE, CANCELLED, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Mark a request's operator deadline as expired.  It shares the same
    /// transport interruption path as cancellation but remains distinguishable
    /// at the MCP boundary for a stable timeout error.
    pub fn timeout(&self) {
        let _ = self
            .state
            .compare_exchange(ACTIVE, TIMED_OUT, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Return whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        matches!(self.state.load(Ordering::Acquire), CANCELLED | TIMED_OUT)
    }

    pub fn is_timed_out(&self) -> bool {
        self.state.load(Ordering::Acquire) == TIMED_OUT
    }

    /// Atomically establish the operation's irreversible commit point.
    /// Returns false exactly when cancellation won first. Once this returns
    /// true, later cancellation cannot kill PET during publication or another
    /// transaction that must run to a consistent boundary.
    fn commit(&self) -> bool {
        match self
            .state
            .compare_exchange(ACTIVE, COMMITTED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) | Err(COMMITTED) => true,
            Err(CANCELLED | TIMED_OUT) => false,
            Err(_) => unreachable!("request cancellation state is invalid"),
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<RequestCancellation>> = const { RefCell::new(None) };
}

struct Restore(Option<RequestCancellation>);

impl Drop for Restore {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT.with(|current| {
            current.replace(previous);
        });
    }
}

/// Run one synchronous engine operation under its MCP cancellation signal.
/// Nested scopes restore the previous signal even if the operation unwinds.
pub fn with_request_cancellation<T>(
    cancellation: &RequestCancellation,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = CURRENT.with(|current| current.replace(Some(cancellation.clone())));
    let _restore = Restore(previous);
    operation()
}

/// Whether the blocking operation on this thread has been cancelled.
pub fn request_cancelled() -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(RequestCancellation::is_cancelled)
    })
}

/// Whether the active request was interrupted by its operator deadline.
pub fn request_timed_out() -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(RequestCancellation::is_timed_out)
    })
}

/// Atomically choose the wrapper transaction over a racing cancellation.
/// Outside an MCP request scope there is no cancellation owner, so direct
/// engine callers may always proceed. Callers must invoke this immediately
/// before their first irreversible checkpoint/session/source transition.
pub fn commit_request() -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_none_or(RequestCancellation::commit)
    })
}

/// Whether the current thread has an MCP request scope. Without one, direct
/// engine callers retain the ordinary blocking transport behavior.
pub(crate) fn request_scope_active() -> bool {
    CURRENT.with(|current| current.borrow().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_and_commit_have_one_atomic_winner() {
        let cancelled = RequestCancellation::new();
        cancelled.cancel();
        assert!(cancelled.is_cancelled());
        assert!(!cancelled.commit());

        let committed = RequestCancellation::new();
        assert!(committed.commit());
        committed.cancel();
        assert!(!committed.is_cancelled());
        assert!(committed.commit());
    }
}
