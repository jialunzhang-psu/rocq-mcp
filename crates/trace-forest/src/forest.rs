//! Concurrent topology, lifecycle, and payload admission.

use crate::{
    ActionKey, CallError, CloseOutcome, Config, CursorId, Error, RootKey, TraceView,
    spill::{SpillPointer, SpillStore},
};
use dashmap::{DashMap, mapref::entry::Entry};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::HashSet,
    fs,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use uuid::Uuid;

/// A generic immutable prefix forest.
///
/// Design note: all topology indexes are sharded `DashMap`s.  Slow caller work
/// and spill I/O happen outside their guards; a small per-root state mutex only
/// serializes lifecycle transitions and publication into that one root family.
pub struct TraceForest<R, A> {
    inner: Arc<Inner>,
    marker: std::marker::PhantomData<fn() -> (R, A)>,
}

impl<R, A> Clone for TraceForest<R, A> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            marker: std::marker::PhantomData,
        }
    }
}

impl<R, A> TraceForest<R, A>
where
    R: Serialize + DeserializeOwned + Send + Sync + 'static,
    A: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// Creates an empty forest and a uniquely owned spill child directory.
    pub fn new(config: Config) -> Result<Self, Error> {
        fs::create_dir_all(&config.spill_parent).map_err(|_| Error::SpillUnavailable)?;
        let directory = config
            .spill_parent
            .join(format!("trace-forest-{}", Uuid::new_v4()));
        fs::create_dir(&directory).map_err(|_| Error::SpillUnavailable)?;
        Ok(Self {
            inner: Arc::new(Inner {
                high_water_bytes: config.high_water_bytes,
                resident_bytes: AtomicUsize::new(0),
                roots: DashMap::new(),
                cursors: DashMap::new(),
                edges: DashMap::new(),
                open_flights: DashMap::new(),
                step_flights: DashMap::new(),
                spill: SpillStore::new(directory),
            }),
            marker: std::marker::PhantomData,
        })
    }

    /// Opens a root, executing `prepare` at most once for concurrent equal keys.
    ///
    /// Followers return the published cursor without executing their callback.
    /// A failed leader publishes nothing; followers receive
    /// [`Error::ConcurrentPreparationFailed`] and may retry.
    pub fn open<E>(
        &self,
        root_key: RootKey,
        prepare: impl FnOnce() -> Result<R, E>,
    ) -> Result<CursorId, CallError<E>> {
        if let Some(root) = self.inner.roots.get(&root_key) {
            let root = Arc::clone(root.value());
            return match root.state().map_err(CallError::Forest)? {
                RootState::Open => Ok(root.cursor),
                RootState::Closing => Err(CallError::Forest(Error::Closing)),
                RootState::Retired => Err(CallError::Forest(Error::Retired)),
            };
        }
        let flight = match self.inner.open_flights.entry(root_key.clone()) {
            Entry::Occupied(entry) => return entry.get().wait().map_err(CallError::Forest),
            Entry::Vacant(entry) => {
                let flight = Arc::new(Flight::new());
                entry.insert(Arc::clone(&flight));
                flight
            }
        };

        // Recheck after winning the flight: a caller can miss a publication,
        // then arrive after its leader removed the completed flight.
        if let Some(root) = self.inner.roots.get(&root_key) {
            let root = Arc::clone(root.value());
            let result = match root.state() {
                Ok(RootState::Open) => Ok(root.cursor),
                Ok(RootState::Closing) => Err(Error::Closing),
                Ok(RootState::Retired) => Err(Error::Retired),
                Err(error) => Err(error),
            };
            flight.finish(result.clone());
            self.remove_open_flight(&root_key, &flight);
            return result.map_err(CallError::Forest);
        }
        // We are this key's only leader. Callback work intentionally occurs
        // after the flight is published and outside every forest lock.
        let prepared = catch_unwind(AssertUnwindSafe(prepare));
        let value = match prepared {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                flight.finish(Err(Error::ConcurrentPreparationFailed));
                self.remove_open_flight(&root_key, &flight);
                return Err(CallError::Callback(error));
            }
            Err(panic) => {
                flight.finish(Err(Error::ConcurrentPreparationFailed));
                self.remove_open_flight(&root_key, &flight);
                resume_unwind(panic);
            }
        };
        let bytes = match encode(&value) {
            Ok(bytes) => bytes,
            Err(error) => {
                flight.finish(Err(error.clone()));
                self.remove_open_flight(&root_key, &flight);
                return Err(CallError::Forest(error));
            }
        };
        let payload = match self.admit_payload(bytes) {
            Ok(payload) => payload,
            Err(error) => {
                flight.finish(Err(error.clone()));
                self.remove_open_flight(&root_key, &flight);
                return Err(CallError::Forest(error));
            }
        };
        let cursor = CursorId(new_uuid());
        let root = Arc::new(RootRecord::new(cursor, root_key.clone(), payload));
        self.inner
            .cursors
            .insert(cursor, NodeRecord::root(Arc::clone(&root)));
        self.inner.roots.insert(root_key.clone(), Arc::clone(&root));
        flight.finish(Ok(cursor));
        self.remove_open_flight(&root_key, &flight);
        Ok(cursor)
    }

    /// Appends one immutable action below `parent`.
    ///
    /// Equal `(parent, action_key)` requests share one callback execution and
    /// child cursor. Callers that need a prefix can explicitly call [`Self::inspect`]
    /// before `step`; append itself never deserializes ancestors.
    pub fn step<E>(
        &self,
        parent: CursorId,
        action_key: ActionKey,
        prepare: impl FnOnce() -> Result<A, E>,
    ) -> Result<CursorId, CallError<E>> {
        let parent_node = self.node(parent).map_err(CallError::Forest)?;
        let root = Arc::clone(&parent_node.root);
        match root.state().map_err(CallError::Forest)? {
            RootState::Open => {}
            RootState::Closing => return Err(CallError::Forest(Error::Closing)),
            RootState::Retired => return Err(CallError::Forest(Error::Retired)),
        }
        let edge_key = EdgeKey {
            parent,
            action: action_key.clone(),
        };
        if let Some(cursor) = self.inner.edges.get(&edge_key) {
            return Ok(*cursor.value());
        }
        let flight = match self.inner.step_flights.entry(edge_key.clone()) {
            Entry::Occupied(entry) => return entry.get().wait().map_err(CallError::Forest),
            Entry::Vacant(entry) => {
                let flight = Arc::new(Flight::new());
                entry.insert(Arc::clone(&flight));
                flight
            }
        };
        // Same recheck closes the post-publication/pre-flight race without
        // invoking a duplicate expensive callback.
        if let Some(cursor) = self.inner.edges.get(&edge_key) {
            let cursor = *cursor.value();
            flight.finish(Ok(cursor));
            self.remove_step_flight(&edge_key, &flight);
            return Ok(cursor);
        }
        match root.state() {
            Ok(RootState::Open) => {}
            Ok(RootState::Closing) => {
                flight.finish(Err(Error::Closing));
                self.remove_step_flight(&edge_key, &flight);
                return Err(CallError::Forest(Error::Closing));
            }
            Ok(RootState::Retired) => {
                flight.finish(Err(Error::Retired));
                self.remove_step_flight(&edge_key, &flight);
                return Err(CallError::Forest(Error::Retired));
            }
            Err(error) => {
                flight.finish(Err(error.clone()));
                self.remove_step_flight(&edge_key, &flight);
                return Err(CallError::Forest(error));
            }
        }
        let prepared = catch_unwind(AssertUnwindSafe(prepare));
        let action = match prepared {
            Ok(Ok(action)) => action,
            Ok(Err(error)) => {
                flight.finish(Err(Error::ConcurrentPreparationFailed));
                self.remove_step_flight(&edge_key, &flight);
                return Err(CallError::Callback(error));
            }
            Err(panic) => {
                flight.finish(Err(Error::ConcurrentPreparationFailed));
                self.remove_step_flight(&edge_key, &flight);
                resume_unwind(panic);
            }
        };
        let bytes = match encode(&action) {
            Ok(bytes) => bytes,
            Err(error) => {
                flight.finish(Err(error.clone()));
                self.remove_step_flight(&edge_key, &flight);
                return Err(CallError::Forest(error));
            }
        };

        let payload = match self.admit_payload(bytes) {
            Ok(payload) => payload,
            Err(error) => {
                flight.finish(Err(error.clone()));
                self.remove_step_flight(&edge_key, &flight);
                return Err(CallError::Forest(error));
            }
        };

        // Design note: checking Open and publishing this edge under the same
        // root lock linearizes step against close without global serialization.
        let child = {
            let lifecycle = match root.lock_state() {
                Ok(lock) => lock,
                Err(error) => {
                    self.discard_unpublished_payload(&payload);
                    flight.finish(Err(error.clone()));
                    self.remove_step_flight(&edge_key, &flight);
                    return Err(CallError::Forest(error));
                }
            };
            if *lifecycle != RootState::Open {
                let error = match *lifecycle {
                    RootState::Closing => Error::Closing,
                    RootState::Retired => Error::Retired,
                    RootState::Open => Error::ConcurrentPreparationFailed,
                };
                self.discard_unpublished_payload(&payload);
                flight.finish(Err(error.clone()));
                self.remove_step_flight(&edge_key, &flight);
                return Err(CallError::Forest(error));
            }
            if let Some(existing) = self.inner.edges.get(&edge_key) {
                let cursor = *existing.value();
                self.discard_unpublished_payload(&payload);
                cursor
            } else {
                // Acquire this remaining fallible lock before any publication.
                let mut members = root
                    .members
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let cursor = CursorId(new_uuid());
                let node = Arc::new(NodeRecord {
                    root: Arc::clone(&root),
                    parent: Some(parent),
                    action_key: Some(action_key),
                    payload,
                    depth: parent_node.depth + 1,
                });
                self.inner.cursors.insert(cursor, Arc::clone(&node));
                self.inner.edges.insert(edge_key.clone(), cursor);
                members.insert(cursor);
                cursor
            }
        };
        flight.finish(Ok(child));
        self.remove_step_flight(&edge_key, &flight);
        Ok(child)
    }

    /// Returns an owned root-to-cursor snapshot without application replay.
    pub fn inspect(&self, cursor: CursorId) -> Result<TraceView<R, A>, Error> {
        let node = self.node(cursor)?;
        match node.root.state()? {
            RootState::Retired => return Err(Error::Retired),
            RootState::Open | RootState::Closing => {}
        }
        let mut reversed = Vec::with_capacity(node.depth);
        let mut current = node;
        loop {
            if let Some(parent) = current.parent {
                reversed.push(current.payload.decode::<A>(&self.inner.spill)?);
                current = self.node(parent)?;
            } else {
                let root = current.root.payload.decode::<R>(&self.inner.spill)?;
                reversed.reverse();
                return Ok(TraceView {
                    root,
                    actions: reversed,
                });
            }
        }
    }

    /// Runs an exclusive per-root terminal effect.
    ///
    /// The effect receives an owned snapshot and never runs under a forest-wide
    /// lock. An error (or panic) restores the root to Open with every branch.
    pub fn close<E>(
        &self,
        cursor: CursorId,
        effect: impl FnOnce(&TraceView<R, A>) -> Result<(), E>,
    ) -> Result<CloseOutcome, CallError<E>> {
        let node = self.node(cursor).map_err(CallError::Forest)?;
        let root = Arc::clone(&node.root);
        {
            let mut state = root.lock_state().map_err(CallError::Forest)?;
            loop {
                match *state {
                    RootState::Open => {
                        *state = RootState::Closing;
                        break;
                    }
                    RootState::Closing => {
                        state = root
                            .cv
                            .wait(state)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    RootState::Retired => return Ok(CloseOutcome::AlreadyRetired),
                }
            }
        }
        let view = match self.inspect_while_closing(cursor, &root) {
            Ok(view) => view,
            Err(error) => {
                root.reopen();
                return Err(CallError::Forest(error));
            }
        };
        match catch_unwind(AssertUnwindSafe(|| effect(&view))) {
            Ok(Ok(())) => {
                self.retire(&root);
                Ok(CloseOutcome::Closed)
            }
            Ok(Err(error)) => {
                root.reopen();
                Err(CallError::Callback(error))
            }
            Err(panic) => {
                root.reopen();
                resume_unwind(panic);
            }
        }
    }

    fn inspect_while_closing(
        &self,
        cursor: CursorId,
        root: &Arc<RootRecord>,
    ) -> Result<TraceView<R, A>, Error> {
        let node = self.node(cursor)?;
        if !Arc::ptr_eq(&node.root, root) {
            return Err(Error::UnknownCursor);
        }
        let mut reversed = Vec::with_capacity(node.depth);
        let mut current = node;
        loop {
            if let Some(parent) = current.parent {
                reversed.push(current.payload.decode::<A>(&self.inner.spill)?);
                current = self.node(parent)?;
            } else {
                let root_value = current.root.payload.decode::<R>(&self.inner.spill)?;
                reversed.reverse();
                return Ok(TraceView {
                    root: root_value,
                    actions: reversed,
                });
            }
        }
    }

    fn retire(&self, root: &Arc<RootRecord>) {
        // State changes before index removal, so a concurrent closer which
        // already holds this root observes AlreadyRetired without retaining a
        // historical cursor index after retirement.
        let members = match root.members.lock() {
            Ok(members) => members.iter().copied().collect::<Vec<_>>(),
            Err(poisoned) => poisoned.into_inner().iter().copied().collect::<Vec<_>>(),
        };
        match root.state.lock() {
            Ok(mut state) => {
                *state = RootState::Retired;
                root.cv.notify_all();
            }
            Err(poisoned) => {
                // A callback never holds this lock; recover rather than strand
                // close waiters if an internal invariant panic poisoned it.
                let mut state = poisoned.into_inner();
                *state = RootState::Retired;
                root.cv.notify_all();
            }
        }
        let mut released = root.payload.resident_len().unwrap_or(0);
        for cursor in members {
            if let Some((_, node)) = self.inner.cursors.remove(&cursor) {
                released = released.saturating_add(node.payload.resident_len().unwrap_or(0));
                if let (Some(parent), Some(action)) = (node.parent, node.action_key.clone()) {
                    self.inner.edges.remove(&EdgeKey { parent, action });
                }
            }
        }
        self.inner
            .resident_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |bytes| {
                Some(bytes.saturating_sub(released))
            })
            .ok();
        self.inner.roots.remove(&root.key);
    }

    fn node(&self, cursor: CursorId) -> Result<Arc<NodeRecord>, Error> {
        if let Some(node) = self.inner.cursors.get(&cursor) {
            Ok(Arc::clone(node.value()))
        } else {
            Err(Error::UnknownCursor)
        }
    }

    fn remove_open_flight(&self, key: &RootKey, flight: &Arc<Flight>) {
        if let Entry::Occupied(entry) = self.inner.open_flights.entry(key.clone())
            && Arc::ptr_eq(entry.get(), flight)
        {
            entry.remove();
        }
    }

    fn remove_step_flight(&self, key: &EdgeKey, flight: &Arc<Flight>) {
        if let Entry::Occupied(entry) = self.inner.step_flights.entry(key.clone())
            && Arc::ptr_eq(entry.get(), flight)
        {
            entry.remove();
        }
    }

    fn admit_payload(&self, bytes: Vec<u8>) -> Result<Arc<PayloadCell>, Error> {
        let byte_len = bytes.len();
        loop {
            let resident = self.inner.resident_bytes.load(Ordering::Acquire);
            if resident
                .checked_add(byte_len)
                .is_some_and(|total| total <= self.inner.high_water_bytes)
            {
                if self
                    .inner
                    .resident_bytes
                    .compare_exchange(
                        resident,
                        resident + byte_len,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return Ok(Arc::new(PayloadCell::resident(bytes)));
                }
                continue;
            }
            // Watermark admission is synchronous but does not fsync. If spill
            // fails, no topology has been published and the caller may retry.
            let pointer = self.inner.spill.write_batch(&[bytes.as_slice()])?.remove(0);
            return Ok(Arc::new(PayloadCell::spilled(pointer)));
        }
    }

    fn discard_unpublished_payload(&self, payload: &Arc<PayloadCell>) {
        if let Ok(bytes) = payload.resident_len() {
            let _ = self.inner.resident_bytes.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |current| Some(current.saturating_sub(bytes)),
            );
        }
    }
}

struct Inner {
    high_water_bytes: usize,
    resident_bytes: AtomicUsize,
    roots: DashMap<RootKey, Arc<RootRecord>>,
    cursors: DashMap<CursorId, Arc<NodeRecord>>,
    edges: DashMap<EdgeKey, CursorId>,
    open_flights: DashMap<RootKey, Arc<Flight>>,
    step_flights: DashMap<EdgeKey, Arc<Flight>>,
    spill: SpillStore,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // This is exactly the unique child made in new(), never its parent.
        let _ = fs::remove_dir_all(&self.spill.directory);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct EdgeKey {
    parent: CursorId,
    action: ActionKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootState {
    Open,
    Closing,
    Retired,
}

/// Metadata shared by all nodes in one root family.
struct RootRecord {
    cursor: CursorId,
    key: RootKey,
    payload: Arc<PayloadCell>,
    state: Mutex<RootState>,
    cv: Condvar,
    members: Mutex<HashSet<CursorId>>,
}

impl RootRecord {
    fn new(cursor: CursorId, key: RootKey, payload: Arc<PayloadCell>) -> Self {
        let mut members = HashSet::new();
        members.insert(cursor);
        Self {
            cursor,
            key,
            payload,
            state: Mutex::new(RootState::Open),
            cv: Condvar::new(),
            members: Mutex::new(members),
        }
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, RootState>, Error> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    fn state(&self) -> Result<RootState, Error> {
        Ok(*self.lock_state()?)
    }

    fn reopen(&self) {
        // Recovering a callback panic is intentional; reopening must not leave
        // a root permanently Closing merely because user code unwound.
        match self.state.lock() {
            Ok(mut state) => {
                *state = RootState::Open;
                self.cv.notify_all();
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                *state = RootState::Open;
                self.cv.notify_all();
            }
        }
    }
}

/// One immutable topology record. Payload bytes alone are spillable.
struct NodeRecord {
    root: Arc<RootRecord>,
    parent: Option<CursorId>,
    action_key: Option<ActionKey>,
    payload: Arc<PayloadCell>,
    depth: usize,
}

impl NodeRecord {
    fn root(root: Arc<RootRecord>) -> Arc<Self> {
        Arc::new(Self {
            root,
            parent: None,
            action_key: None,
            payload: Arc::new(PayloadCell::resident(Vec::new())),
            depth: 0,
        })
    }
}

#[derive(Clone)]
enum PayloadSlot {
    Resident(Vec<u8>),
    Spilled(SpillPointer),
}

struct PayloadCell {
    slot: Mutex<PayloadSlot>,
}

impl PayloadCell {
    fn resident(bytes: Vec<u8>) -> Self {
        Self {
            slot: Mutex::new(PayloadSlot::Resident(bytes)),
        }
    }
    fn spilled(pointer: SpillPointer) -> Self {
        Self {
            slot: Mutex::new(PayloadSlot::Spilled(pointer)),
        }
    }
    fn resident_len(&self) -> Result<usize, Error> {
        match &*self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            PayloadSlot::Resident(bytes) => Ok(bytes.len()),
            PayloadSlot::Spilled(_) => Ok(0),
        }
    }
    fn decode<T: DeserializeOwned>(&self, spill: &SpillStore) -> Result<T, Error> {
        let slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let bytes = match slot {
            PayloadSlot::Resident(bytes) => bytes,
            PayloadSlot::Spilled(pointer) => spill.read(&pointer)?,
        };
        decode(&bytes)
    }
}

struct Flight {
    state: Mutex<FlightState>,
    cv: Condvar,
}

enum FlightState {
    Running,
    Finished(Result<CursorId, Error>),
}

impl Flight {
    fn new() -> Self {
        Self {
            state: Mutex::new(FlightState::Running),
            cv: Condvar::new(),
        }
    }

    fn finish(&self, result: Result<CursorId, Error>) {
        match self.state.lock() {
            Ok(mut state) => {
                *state = FlightState::Finished(result);
                self.cv.notify_all();
            }
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                *state = FlightState::Finished(result);
                self.cv.notify_all();
            }
        }
    }

    fn wait(&self) -> Result<CursorId, Error> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match &*state {
                FlightState::Finished(result) => return result.clone(),
                FlightState::Running => {
                    state = self
                        .cv
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
            }
        }
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::PayloadCodec)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|_| Error::PayloadCodec)
}

fn new_uuid() -> Uuid {
    // UUIDv7 improves index and segment locality; topology still uses parents.
    Uuid::now_v7()
}
