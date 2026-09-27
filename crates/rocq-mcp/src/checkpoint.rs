//! Request-boundary checkpoints for one MCP connection.
//!
//! PET and trace-forest retain sentence-level proof states. This module adds
//! only the user-visible topology of successful `check` requests; it neither
//! interprets Rocq commands nor owns proof state.

use std::collections::BTreeMap;

/// Session-local, monotonically allocated checkpoint identifier.
///
/// IDs are never reused during a connection, including after switching proofs.
/// Their numeric encoding is intentionally isolated in this module so it can
/// be replaced without changing engine or trace-forest contracts.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CheckpointId(u64);

impl CheckpointId {
    pub(crate) fn from_u64(value: u64) -> Self {
        Self(value)
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

/// Failures of the connection-local checkpoint graph or its non-reusing ID allocator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CheckpointError {
    NoActiveProof,
    Unknown,
    TooFar,
    Exhausted,
}

/// One request boundary and the opaque engine handle selected there.
struct Entry<H> {
    handle: H,
    parent: Option<CheckpointId>,
}

/// Active proof checkpoint graph plus a connection-lifetime ID allocator.
///
/// Only entries for the active proof are addressable. Clearing or beginning a
/// proof drops those entries but deliberately preserves `next`, making stale
/// checkpoint IDs invalid rather than aliases for a later proof.
pub(crate) struct CheckpointBook<H> {
    next: Option<u64>,
    entries: BTreeMap<CheckpointId, Entry<H>>,
    current: Option<CheckpointId>,
}

impl<H> Default for CheckpointBook<H> {
    fn default() -> Self {
        Self {
            next: Some(1),
            entries: BTreeMap::new(),
            current: None,
        }
    }
}

impl<H: Copy + Eq> CheckpointBook<H> {
    /// Start a new active proof at `handle` and return its root checkpoint.
    pub(crate) fn begin(&mut self, handle: H) -> Result<CheckpointId, CheckpointError> {
        let id = self.allocate()?;
        self.entries.clear();
        self.entries.insert(
            id,
            Entry {
                handle,
                parent: None,
            },
        );
        self.current = Some(id);
        Ok(id)
    }

    /// Record one `check` request boundary.
    ///
    /// An all-rejected request leaves the engine handle unchanged and therefore
    /// reuses the current checkpoint. A selected fragment gets one fresh child
    /// ID, even when it contains several sentences or another branch already
    /// contains the resulting handle.
    pub(crate) fn commit(&mut self, handle: H) -> Result<CheckpointId, CheckpointError> {
        let parent = self.current.ok_or(CheckpointError::NoActiveProof)?;
        let current = self.entries.get(&parent).ok_or(CheckpointError::Unknown)?;
        if current.handle == handle {
            return Ok(parent);
        }
        let id = self.allocate()?;
        self.entries.insert(
            id,
            Entry {
                handle,
                parent: Some(parent),
            },
        );
        self.current = Some(id);
        Ok(id)
    }

    /// Return the currently selected checkpoint and engine handle.
    pub(crate) fn current(&self) -> Option<(CheckpointId, H)> {
        let id = self.current?;
        self.entries.get(&id).map(|entry| (id, entry.handle))
    }

    /// Resolve, without selecting, the ancestor `steps` request boundaries back.
    pub(crate) fn steps_back(&self, steps: u64) -> Result<(CheckpointId, H), CheckpointError> {
        if steps == 0 {
            return Err(CheckpointError::TooFar);
        }
        let mut id = self.current.ok_or(CheckpointError::NoActiveProof)?;
        for _ in 0..steps {
            id = self
                .entries
                .get(&id)
                .ok_or(CheckpointError::Unknown)?
                .parent
                .ok_or(CheckpointError::TooFar)?;
        }
        self.lookup(id)
    }

    /// Resolve an exact active-proof checkpoint without changing selection.
    pub(crate) fn lookup(&self, id: CheckpointId) -> Result<(CheckpointId, H), CheckpointError> {
        self.entries
            .get(&id)
            .map(|entry| (id, entry.handle))
            .ok_or(CheckpointError::Unknown)
    }

    /// Select a previously resolved checkpoint after engine checkout succeeds.
    pub(crate) fn select(&mut self, id: CheckpointId) -> Result<(), CheckpointError> {
        if !self.entries.contains_key(&id) {
            return Err(CheckpointError::Unknown);
        }
        self.current = Some(id);
        Ok(())
    }

    /// Forget the active proof while preserving connection-wide ID monotonicity.
    pub(crate) fn clear_active(&mut self) {
        self.entries.clear();
        self.current = None;
    }

    fn allocate(&mut self) -> Result<CheckpointId, CheckpointError> {
        let value = self.next.ok_or(CheckpointError::Exhausted)?;
        self.next = value.checked_add(1);
        Ok(CheckpointId(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_remain_monotonic_across_proofs() {
        let mut book = CheckpointBook::default();
        assert_eq!(book.begin("a").unwrap().get(), 1);
        assert_eq!(book.commit("b").unwrap().get(), 2);
        book.clear_active();
        assert_eq!(book.begin("c").unwrap().get(), 3);
        assert_eq!(
            book.lookup(CheckpointId::from_u64(1)),
            Err(CheckpointError::Unknown)
        );
    }

    #[test]
    fn unchanged_handle_does_not_allocate_a_boundary() {
        let mut book = CheckpointBook::default();
        let root = book.begin(10).unwrap();
        assert_eq!(book.commit(10).unwrap(), root);
        assert_eq!(book.commit(11).unwrap().get(), 2);
    }

    #[test]
    fn one_and_many_step_rewind_follow_request_parents() {
        let mut book = CheckpointBook::default();
        let root = book.begin(0).unwrap();
        let one = book.commit(1).unwrap();
        let two = book.commit(2).unwrap();
        assert_eq!(book.steps_back(1).unwrap(), (one, 1));
        assert_eq!(book.steps_back(2).unwrap(), (root, 0));
        assert_eq!(book.steps_back(3), Err(CheckpointError::TooFar));
        assert_eq!(book.current(), Some((two, 2)));
    }

    #[test]
    fn exact_selection_preserves_old_branches() {
        let mut book = CheckpointBook::default();
        let root = book.begin(0).unwrap();
        let old = book.commit(1).unwrap();
        book.select(root).unwrap();
        let branch = book.commit(2).unwrap();
        assert_ne!(old, branch);
        assert_eq!(book.lookup(old).unwrap(), (old, 1));
        assert_eq!(book.lookup(branch).unwrap(), (branch, 2));
        assert_eq!(book.steps_back(1).unwrap(), (root, 0));
    }

    #[test]
    fn equal_engine_handles_keep_distinct_request_histories() {
        let mut book = CheckpointBook::default();
        let root = book.begin(0).unwrap();
        let direct = book.commit(2).unwrap();
        book.select(root).unwrap();
        let intermediate = book.commit(1).unwrap();
        let split = book.commit(2).unwrap();
        assert_ne!(direct, split);

        book.select(direct).unwrap();
        assert_eq!(book.steps_back(1).unwrap(), (root, 0));
        book.select(split).unwrap();
        assert_eq!(book.steps_back(1).unwrap(), (intermediate, 1));
    }

    #[test]
    fn invalid_targets_are_explicit() {
        let mut book = CheckpointBook::<u8>::default();
        assert_eq!(book.steps_back(1), Err(CheckpointError::NoActiveProof));
        let root = book.begin(0).unwrap();
        assert_eq!(book.steps_back(0), Err(CheckpointError::TooFar));
        assert_eq!(book.steps_back(1), Err(CheckpointError::TooFar));
        assert_eq!(
            book.lookup(CheckpointId::from_u64(root.get() + 1)),
            Err(CheckpointError::Unknown)
        );
    }

    #[test]
    fn exhausted_allocator_never_reuses_an_id() {
        let mut book = CheckpointBook::<u8> {
            next: Some(u64::MAX),
            ..CheckpointBook::default()
        };
        assert_eq!(book.begin(0).unwrap().get(), u64::MAX);
        assert_eq!(book.commit(1), Err(CheckpointError::Exhausted));
        assert_eq!(book.current().map(|(id, _)| id.get()), Some(u64::MAX));
    }
}
