//! A concurrent, process-lifetime prefix forest with a spill tier.
//!
//! `trace-forest` deliberately stores only caller-defined roots and actions.  It
//! is not an event log: successful `open` and `step` calls are acknowledged in
//! memory, and all open traces disappear when the process exits.

mod forest;
mod spill;

pub use forest::TraceForest;

use std::{fmt, path::PathBuf, sync::Arc};
use uuid::Uuid;

const MAX_KEY_BYTES: usize = 4096;

/// An opaque service-layer capability identifying one immutable prefix.
///
/// Its UUID is intentionally not a protocol-level identity.  Parent links, not
/// UUID ordering, define the order of a trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct CursorId(pub(crate) Uuid);

impl CursorId {
    /// Returns the internal UUID for service-layer storage and indexing.
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

/// Canonical bytes identifying a root retry within this forest's lifetime.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct RootKey(Arc<[u8]>);

impl RootKey {
    /// Creates a bounded, non-empty canonical root identity.
    pub fn new(bytes: impl AsRef<[u8]>) -> Result<Self, Error> {
        let bytes = bytes.as_ref();
        if bytes.is_empty() || bytes.len() > MAX_KEY_BYTES {
            return Err(Error::InvalidKey);
        }
        Ok(Self(Arc::from(bytes)))
    }

    /// Creates a root key from a fixed cryptographic digest.
    ///
    /// A 32-byte digest is non-empty and below the key bound by construction,
    /// so callers do not need to fabricate an impossible error path.
    pub fn from_digest(bytes: [u8; 32]) -> Self {
        Self(Arc::from(bytes))
    }

    /// Returns the canonical identity bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Canonical bytes identifying an action below one particular parent cursor.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ActionKey(Arc<[u8]>);

impl ActionKey {
    /// Creates a bounded, non-empty canonical action identity.
    pub fn new(bytes: impl AsRef<[u8]>) -> Result<Self, Error> {
        let bytes = bytes.as_ref();
        if bytes.is_empty() || bytes.len() > MAX_KEY_BYTES {
            return Err(Error::InvalidKey);
        }
        Ok(Self(Arc::from(bytes)))
    }

    /// Returns the canonical identity bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// Capacity settings for a [`TraceForest`].
#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) high_water_bytes: usize,
    pub(crate) spill_parent: PathBuf,
}

impl Config {
    /// Makes a configuration whose payload target is `high_water_bytes`.
    ///
    /// The forest creates one unique child beneath `spill_parent` and only ever
    /// removes that child on drop.
    pub fn new(high_water_bytes: usize, spill_parent: impl Into<PathBuf>) -> Self {
        Self {
            high_water_bytes,
            spill_parent: spill_parent.into(),
        }
    }

    /// Returns the resident encoded-payload target.
    pub fn high_water_bytes(&self) -> usize {
        self.high_water_bytes
    }
}

/// Errors produced by forest bookkeeping, codec, or spill operations.
///
/// This type deliberately contains neither payload data nor filesystem paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// A caller supplied an empty or overlong canonical key.
    InvalidKey,
    /// The cursor is not known to this forest.
    UnknownCursor,
    /// A concurrently held root family was successfully closed and retired.
    Retired,
    /// The root family currently has a close effect in flight.
    Closing,
    /// A same-key callback failed in another concurrent request; retry it.
    ConcurrentPreparationFailed,
    /// Generic payload encoding or decoding failed.
    PayloadCodec,
    /// A spill segment was unreadable for an operational reason.
    SpillUnavailable,
    /// A spill segment failed its framing, length, or checksum validation.
    CorruptSpill,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trace forest error: {self:?}")
    }
}

impl std::error::Error for Error {}

/// Separates an application callback failure from a forest failure.
#[derive(Debug, PartialEq, Eq)]
pub enum CallError<E> {
    /// The caller's preparation or close callback returned this error.
    Callback(E),
    /// Forest bookkeeping, spill, or lifecycle failure.
    Forest(Error),
}

impl<E: fmt::Display> fmt::Display for CallError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Callback(error) => write!(f, "callback failed: {error}"),
            Self::Forest(error) => error.fmt(f),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for CallError<E> {}

/// The result of close arbitration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseOutcome {
    /// This caller ran the effect and retired the whole root family.
    Closed,
    /// Another simultaneous close already retired the family.
    AlreadyRetired,
}

/// An owned, replay-free snapshot of a root and its actions in forward order.
#[derive(Debug, PartialEq, Eq)]
pub struct TraceView<R, A> {
    pub(crate) root: R,
    pub(crate) actions: Vec<A>,
}

impl<R, A> TraceView<R, A> {
    /// Returns the root payload.
    pub fn root(&self) -> &R {
        &self.root
    }

    /// Returns actions from the root through the inspected cursor.
    pub fn actions(&self) -> &[A] {
        &self.actions
    }

    /// Splits this owned snapshot into its root and ordered action sequence.
    pub fn into_parts(self) -> (R, Vec<A>) {
        (self.root, self.actions)
    }
}
