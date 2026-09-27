use super::*;

/// One project-local PET checkout. `busy` covers a complete replay, so one
/// checkout never interleaves documents or proof commands from two attempts.
struct ProjectLane {
    process: Arc<PetProcess>,
    /// One PET document/state lane is affinity-bound to a theorem root. This
    /// preserves same-root single-flight while allowing independent roots to
    /// occupy the remaining project pool capacity.
    root_affinity: String,
    busy: AtomicBool,
    /// Only the most recent replay is retained. This is sufficient for
    /// same-root single-flight without retaining historical PET instances.
    cached: Mutex<Option<CachedReplay>>,
    /// Fingerprint of the Dune-reported project inputs trusted by this
    /// PET process. Tactic changes do not change this key.
    document_key: Mutex<Option<[u8; 32]>>,
    rotate_before_reuse: AtomicBool,
}

#[derive(Clone)]
struct CachedReplay {
    key: [u8; 32],
    state: PetState,
}

struct ProjectPoolState {
    lanes: Vec<Arc<ProjectLane>>,
    spawning: bool,
    detached: bool,
}

/// A project owns a pool of disposable lanes rather than one process. This is
/// the concurrency boundary: independent attempts can acquire independent
/// lanes while an individual lane remains serial.
struct ProjectPool {
    state: Mutex<ProjectPoolState>,
    changed: Condvar,
}

impl ProjectPool {
    fn new() -> Self {
        Self {
            state: Mutex::new(ProjectPoolState {
                lanes: Vec::new(),
                spawning: false,
                detached: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn release(&self, lane: &Arc<ProjectLane>, capacity: &Capacity) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(current) = state
            .lanes
            .iter_mut()
            .find(|current| Arc::ptr_eq(current, lane))
        {
            // Design note: a lane is released exactly once by its lease; a
            // removed/invalidated lane is intentionally ignored here.
            current.busy.store(false, Ordering::Release);
            self.changed.notify_all();
            capacity.wake();
        }
    }

    fn remove_lane(&self, lane: &Arc<ProjectLane>) -> Option<Arc<PetProcess>> {
        let removed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let index = state
                .lanes
                .iter()
                .position(|current| Arc::ptr_eq(current, lane))?;
            Some(state.lanes.remove(index).process.clone())
        };
        self.changed.notify_all();
        removed
    }

    fn detach(&self, capacity: &Capacity) {
        let lanes = self.state.lock().map_or_else(
            |poisoned| {
                let mut state = poisoned.into_inner();
                state.detached = true;
                std::mem::take(&mut state.lanes)
            },
            |mut state| {
                state.detached = true;
                std::mem::take(&mut state.lanes)
            },
        );
        self.changed.notify_all();
        for lane in lanes {
            lane.process.terminate();
            capacity.release();
        }
        capacity.wake();
    }

    fn reap_dead_idle(&self) -> usize {
        let mut removed = Vec::new();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.lanes.retain(|lane| {
            let dead = !lane.busy.load(Ordering::Acquire) && lane.process.exited();
            if dead {
                removed.push(lane.process.clone());
            }
            !dead
        });
        if !removed.is_empty() {
            self.changed.notify_all();
        }
        removed.len()
    }

    fn evict_idle(&self) -> Option<Arc<PetProcess>> {
        let removed = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let index = state.lanes.iter().position(|lane| {
                !lane.busy.load(Ordering::Acquire) && lane.process.serial.try_lock().is_ok()
            })?;
            Some(state.lanes.remove(index).process.clone())
        };
        if removed.is_some() {
            self.changed.notify_all();
        }
        removed
    }
}

struct PetLease {
    pool: Arc<ProjectPool>,
    lane: Arc<ProjectLane>,
    capacity: Arc<Capacity>,
}

impl PetLease {
    fn process(&self) -> &Arc<PetProcess> {
        &self.lane.process
    }
}

impl Drop for PetLease {
    fn drop(&mut self) {
        self.pool.release(&self.lane, &self.capacity);
    }
}

/// Capacity-accounted one-call PET process used for source inspection.
struct DisposablePet {
    process: Arc<PetProcess>,
    capacity: Arc<Capacity>,
}

impl DisposablePet {
    fn process(&self) -> &Arc<PetProcess> {
        &self.process
    }
}

impl Drop for DisposablePet {
    fn drop(&mut self) {
        self.process.terminate();
        self.capacity.release();
    }
}

struct Capacity {
    active: Mutex<usize>,
    changed: Condvar,
    maximum: usize,
}

impl Capacity {
    fn reserve_with_reaper(
        &self,
        timeout: Duration,
        mut reclaim: impl FnMut() -> bool,
    ) -> Result<(), PetError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        loop {
            let mut active = self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *active < self.maximum {
                *active += 1;
                return Ok(());
            }
            drop(active);
            // An idle lane can be discarded without disturbing any active
            // replay. This keeps a full pool from turning into an infinite
            // wait when another project needs a checkout.
            if reclaim() {
                continue;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(PetError::Timeout);
            }
            let active = self
                .active
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *active < self.maximum {
                drop(active);
                continue;
            }
            let wait = remaining.min(Duration::from_millis(100));
            let (guard, _timeout) = self
                .changed
                .wait_timeout(active, wait)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(guard);
        }
    }

    fn release(&self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active = active.saturating_sub(1);
        self.changed.notify_all();
    }

    fn wake(&self) {
        self.changed.notify_all();
    }
}

/// Project-indexed lanes with global process capacity.  The capacity mutex is
/// held only for reservation accounting; process I/O never runs under it.
pub(crate) struct PetRuntime {
    timeout: Duration,
    /// Resolved once at engine construction so one runtime cannot mix PET
    /// implementations if its parent process environment later changes.
    pet_binary: PathBuf,
    lanes: Mutex<BTreeMap<PathBuf, Arc<ProjectPool>>>,
    capacity: Arc<Capacity>,
    cache_bytes: usize,
}

impl PetRuntime {
    pub(crate) fn new(
        timeout: Duration,
        max_processes: usize,
        cache_bytes: usize,
    ) -> Result<Self, PetError> {
        if !(1..=64).contains(&max_processes) {
            return Err(PetError::Invalid(
                "max PET processes must be between 1 and 64".into(),
            ));
        }
        Ok(Self {
            timeout,
            pet_binary: configured_pet_binary(),
            lanes: Mutex::new(BTreeMap::new()),
            capacity: Arc::new(Capacity {
                active: Mutex::new(0),
                changed: Condvar::new(),
                maximum: max_processes,
            }),
            cache_bytes,
        })
    }

    /// Execute one replay-safe PET operation under the runtime's single
    /// transport-recovery policy.
    ///
    /// Process loss is retried on a fresh lane/process until the operation
    /// budget is exhausted. A malformed protocol/state response gets exactly
    /// one clean retry. Rocq rejections, invalid input, environment failures,
    /// and the per-RPC watchdog are semantic/final outcomes and are returned
    /// unchanged. The operation must discard or invalidate any failed PET
    /// owner before returning a recoverable error.
    fn recover<T>(
        &self,
        mut operation: impl FnMut() -> Result<T, PetError>,
    ) -> Result<T, PetError> {
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);
        let mut clean_retry_used = false;
        loop {
            match operation() {
                Err(PetError::ProcessFailure(_)) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(PetError::ProcessFailure(_)) => return Err(PetError::Timeout),
                Err(PetError::Protocol(_) | PetError::OutputOverflow | PetError::Stale)
                    if !clean_retry_used =>
                {
                    clean_retry_used = true;
                }
                result => return result,
            }
        }
    }

    /// Run a replay-safe operation in a short-lived PET process. Every retry
    /// owns a new process, and `DisposablePet::drop` reaps the failed attempt
    /// before the next one starts.
    fn with_disposable<T>(
        &self,
        workspace_root: &Path,
        mut operation: impl FnMut(&PetProcess) -> Result<T, PetError>,
    ) -> Result<T, PetError> {
        self.recover(|| {
            let process = self.disposable(workspace_root.to_owned())?;
            operation(process.process())
        })
    }

    fn replay(
        &self,
        project: &Path,
        root: &DeclarationSource,
        actions: &[CanonicalTactic],
    ) -> Result<PetState, PetError> {
        self.recover(|| self.replay_once(project, root, actions))
    }

    /// Return the exact requested trace prefix, reusing its opaque live handle
    /// when possible and otherwise replaying the prefix under the runtime's
    /// recovery policy. Engine never coordinates cache lookup and replay.
    pub(crate) fn restore_state(
        &self,
        project: &Path,
        root: &DeclarationSource,
        preferred: Option<(u64, u64)>,
        actions: &[CanonicalTactic],
    ) -> Result<PetState, PetError> {
        if let Some((instance_epoch, st)) = preferred {
            match self.cached_state(project, root, instance_epoch, st) {
                Ok(state) => return Ok(state),
                Err(error) if state_was_lost(&error) => {}
                Err(error) => return Err(error),
            }
        }
        self.replay(project, root, actions)
    }

    /// Evaluate one sentence from an existing PET state without changing the
    /// lane's authoritative cached state. PET states are persistent values, so
    /// independent proof fragments may safely share the same parent.
    pub(crate) fn fork_state(
        &self,
        project: &Path,
        root: &DeclarationSource,
        state: &PetState,
        actions: &[CanonicalTactic],
        tactic: &CanonicalTactic,
    ) -> Result<PetState, PetError> {
        self.recover_state_operation(project, root, actions, Some(state), |process, state| {
            process.run_tactic(state, tactic)
        })
        .map(|(next, _)| next)
    }

    /// Promote an already evaluated persistent state to the lane's replay
    /// cache after the engine commits the corresponding trace prefix.
    ///
    /// This is cache bookkeeping only: a concurrently lost PET process is
    /// ignored because TraceForest remains authoritative and later restore
    /// will replay `actions`. No process is created and no tactic is rerun.
    pub(crate) fn retain_state(
        &self,
        project: &Path,
        state: &PetState,
        actions: &[CanonicalTactic],
    ) {
        let Ok(project) = fs::canonicalize(project) else {
            return;
        };
        let pool = self.pool(project);
        let lane = {
            let pool_state = pool
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pool_state.detached {
                return;
            }
            let Some(lane) = pool_state
                .lanes
                .iter()
                .find(|lane| Arc::ptr_eq(&lane.process, &state.process))
                .cloned()
            else {
                return;
            };
            // Cache promotion is never allowed to delay a completed semantic
            // operation behind an unrelated user of the same PET lane.
            if lane
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return;
            }
            lane
        };
        let lease = PetLease {
            pool,
            lane,
            capacity: Arc::clone(&self.capacity),
        };
        if lease.lane.process.exited() {
            return;
        }
        if state.instance_epoch != state.process.instance_epoch.load(Ordering::Acquire) {
            return;
        }
        let Some(document_key) = *lease
            .lane
            .document_key
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        else {
            return;
        };
        *lease
            .lane
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CachedReplay {
            key: replay_key(document_key, actions),
            state: state.clone(),
        });
        let replay_bytes = actions
            .iter()
            .map(|action| action.0.len())
            .sum::<usize>()
            .saturating_add(64);
        lease
            .lane
            .rotate_before_reuse
            .store(replay_bytes > self.cache_bytes, Ordering::Release);
    }

    /// Recover the live PET object referenced by an engine-owned opaque
    /// handle. This is a cache lookup only; it never replays a trace.
    fn cached_state(
        &self,
        project: &Path,
        root: &DeclarationSource,
        instance_epoch: u64,
        st: u64,
    ) -> Result<PetState, PetError> {
        let project = fs::canonicalize(project)
            .map_err(|_| PetError::Environment("project is unavailable".into()))?;
        let pool = self.pool(project.clone());
        let lease = self.acquire(&pool, None, root_affinity(root))?;
        let cached = lease
            .lane
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(PetError::Stale)?;
        if cached.state.instance_epoch == instance_epoch && cached.state.st == st {
            Ok(cached.state)
        } else {
            Err(PetError::Stale)
        }
    }

    fn replay_once(
        &self,
        project: &Path,
        root: &DeclarationSource,
        actions: &[CanonicalTactic],
    ) -> Result<PetState, PetError> {
        let project = fs::canonicalize(project)
            .map_err(|_| PetError::Environment("project is unavailable".into()))?;
        let document = match pet_document(&project, root, self.timeout) {
            Ok(document) => document,
            Err(error) => {
                // Design note: once the latest source is incompatible, no PET
                // in this project may remain a cache authority. A later source
                // restoration must therefore perform a fresh replay.
                self.detach(&project);
                return Err(PetError::Invalid(error.message));
            }
        };
        let document_key = document.key;
        let replay_key = replay_key(document_key, actions);
        let pool = self.pool(project);
        let affinity = root_affinity(root);
        let mut lease = self.acquire(
            &pool,
            Some(document.workspace_root.clone()),
            affinity.clone(),
        )?;
        if lease.lane.rotate_before_reuse.swap(false, Ordering::AcqRel) {
            self.invalidate(&pool, &lease.lane);
            drop(lease);
            lease = self.acquire(
                &pool,
                Some(document.workspace_root.clone()),
                affinity.clone(),
            )?;
        }
        let inputs_changed = {
            let mut previous = lease
                .lane
                .document_key
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match *previous {
                Some(old) if old != document_key => true,
                _ => {
                    *previous = Some(document_key);
                    false
                }
            }
        };
        if inputs_changed {
            // Design note: setWorkspace cannot revoke already-loaded .vo
            // definitions inside a live PET. A changed Dune input must
            // get a fresh process before any cached or native state is used.
            self.invalidate(&pool, &lease.lane);
            drop(lease);
            lease = self.acquire(&pool, Some(document.workspace_root.clone()), affinity)?;
            *lease
                .lane
                .document_key
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(document_key);
        }
        if let Some(cached) = lease
            .lane
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|cached| cached.key == replay_key)
            .cloned()
        {
            return Ok(cached.state);
        }
        let result = lease.process().replay(document, root, actions);
        match result {
            Ok(state) => {
                *lease
                    .lane
                    .cached
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CachedReplay {
                    key: replay_key,
                    state: state.clone(),
                });
                let replay_bytes = actions
                    .iter()
                    .map(|action| action.0.len())
                    .sum::<usize>()
                    .saturating_add(64);
                lease
                    .lane
                    .rotate_before_reuse
                    .store(replay_bytes > self.cache_bytes, Ordering::Release);
                Ok(state)
            }
            Err(error) => {
                if is_fatal(&error) {
                    self.invalidate(&pool, &lease.lane);
                }
                Err(error)
            }
        }
    }

    pub(crate) fn run_fixed(
        &self,
        project: &Path,
        root: &DeclarationSource,
        state: &PetState,
        actions: &[CanonicalTactic],
        query: FixedPetQuery,
    ) -> Result<(String, PetState), PetError> {
        self.recover_state_operation(project, root, actions, Some(state), |process, state| {
            process.run_fixed(state, query.clone())
        })
    }

    /// Close a solved persistent PET state and query its kernel assumptions.
    /// This evolves only functional PET state; it neither edits nor mirrors
    /// the project source.
    pub(crate) fn candidate_assumptions(
        &self,
        project: &Path,
        state: &PetState,
        declaration: &DeclarationSource,
        actions: &[CanonicalTactic],
    ) -> Result<PetAssumptionReport, PetError> {
        self.recover_state_operation(
            project,
            declaration,
            actions,
            Some(state),
            |process, state| {
                process.close_and_query_assumptions(
                    state,
                    declaration.info.kind.terminator(),
                    declaration.info.identity.constant().unwrap_or_default(),
                )
            },
        )
        .map(|(report, _)| report)
    }

    /// Run an operation against a preferred live state, then reconstruct that
    /// exact immutable trace prefix when the state owner is unavailable. This
    /// is the only recovery loop for stateful PET RPCs.
    fn recover_state_operation<T>(
        &self,
        project: &Path,
        root: &DeclarationSource,
        actions: &[CanonicalTactic],
        preferred: Option<&PetState>,
        mut operation: impl FnMut(&PetProcess, &PetState) -> Result<T, PetError>,
    ) -> Result<(T, PetState), PetError> {
        let mut preferred = preferred.cloned();
        self.recover(|| {
            let state = match preferred.take() {
                Some(state) => state,
                None => self.replay_once(project, root, actions)?,
            };
            let output =
                self.with_live_state(project, root, &state, |process| operation(process, &state))?;
            Ok((output, state))
        })
    }

    /// Lease the exact state owner for one RPC and invalidate that lane on a
    /// fatal transport/protocol result before recovery can start elsewhere.
    fn with_live_state<T>(
        &self,
        project: &Path,
        _root: &DeclarationSource,
        state: &PetState,
        operation: impl FnOnce(&PetProcess) -> Result<T, PetError>,
    ) -> Result<T, PetError> {
        let lease = self.lease_state(project, state)?;
        let result = operation(lease.process());
        if result.as_ref().err().is_some_and(is_fatal) {
            self.invalidate(&lease.pool, &lease.lane);
        }
        result
    }

    /// Pin the lane behind an opaque state for the complete native operation.
    /// This prevents global-capacity eviction from killing PET between cache
    /// lookup and a query/candidate/close RPC.
    fn lease_state(&self, project: &Path, state: &PetState) -> Result<PetLease, PetError> {
        let project = fs::canonicalize(project)
            .map_err(|_| PetError::Environment("project is unavailable".into()))?;
        let pool = self.pool(project.clone());
        // Design note: an opaque PET state is affine to the exact lane that
        // created it. Acquiring any idle lane with the same root affinity is
        // insufficient: it would report a false stale handle and spawn an
        // unnecessary replay process for every read-only fork.
        let lease = self.acquire_exact(&pool, &state.process)?;
        // The cached prefix may have been retired by a sibling writeback, but
        // a solved branch still legitimately owns the same live functional
        // PET process. Validate the opaque process/epoch directly instead of
        // requiring the lane cache to retain that branch.
        if !Arc::ptr_eq(&lease.lane.process, &state.process)
            || state.instance_epoch != state.process.instance_epoch.load(Ordering::Acquire)
        {
            return Err(PetError::Stale);
        }
        Ok(lease)
    }

    /// Lease one exact PET process without manufacturing or selecting a
    /// sibling lane. The caller owns an opaque state whose process is known;
    /// waiting for that lane is bounded by the normal PET operation timeout.
    fn acquire_exact(
        &self,
        pool: &Arc<ProjectPool>,
        process: &Arc<PetProcess>,
    ) -> Result<PetLease, PetError> {
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);
        loop {
            let state = pool
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.detached {
                return Err(PetError::Stale);
            }
            let Some(lane) = state
                .lanes
                .iter()
                .find(|lane| Arc::ptr_eq(&lane.process, process))
                .cloned()
            else {
                return Err(PetError::Stale);
            };
            if lane.process.exited() {
                return Err(PetError::Stale);
            }
            if lane
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Ok(PetLease {
                    pool: Arc::clone(pool),
                    lane,
                    capacity: Arc::clone(&self.capacity),
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(PetError::Timeout);
            }
            let (next, _timeout) = pool
                .changed
                .wait_timeout(state, remaining.min(Duration::from_millis(100)))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            drop(next);
        }
    }

    /// Run a fixed query immediately after an unchanged declaration in its
    /// original source context. One recoverable disposable PET owns both the
    /// full-span lookup and the query, so callers never coordinate two PET
    /// process lifetimes themselves.
    pub(crate) fn query_after_declaration(
        &self,
        project: &Path,
        source: &DeclarationSource,
        query: FixedPetQuery,
    ) -> Result<String, PetError> {
        let workspace = self.workspace_for_source(project, &source.anchor.source)?;
        self.with_disposable(&workspace, |process| {
            let source = process.source_span(source)?;
            let offset = source
                .anchor
                .declaration
                .as_ref()
                .ok_or_else(|| PetError::Protocol("PET declaration range is unavailable".into()))?
                .end;
            process.query_source(
                &source.anchor.source,
                offset,
                source.anchor.digest,
                Some((
                    source.info.identity.constant().unwrap_or_default(),
                    source.info.kind,
                    source.anchor.header.end,
                )),
                query.clone(),
            )
        })
    }

    /// Return PET-resolved kernel assumptions for an existing declaration at
    /// its checked source position. Printed short names never leave PET.
    pub(crate) fn source_assumptions(
        &self,
        project: &Path,
        source: &Path,
        offset: usize,
        expected_digest: [u8; 32],
        expected_header: Option<(&str, DeclarationKind, usize)>,
        name: &str,
    ) -> Result<PetAssumptionReport, PetError> {
        let workspace = self.workspace_for_source(project, source)?;
        self.with_disposable(&workspace, |process| {
            process.source_assumptions(source, offset, expected_digest, expected_header, name)
        })
    }

    pub(crate) fn source_proof_completed(
        &self,
        project: &Path,
        source: &DeclarationSource,
    ) -> Result<bool, PetError> {
        let end = source
            .anchor
            .declaration
            .as_ref()
            .ok_or_else(|| PetError::Protocol("PET declaration range is unavailable".into()))?
            .end;
        let workspace = self.workspace_for_source(project, &source.anchor.source)?;
        self.with_disposable(&workspace, |process| {
            process.source_proof_completed(&source.anchor.source, end, source.anchor.digest)
        })
    }

    pub(crate) fn source_span(
        &self,
        project: &Path,
        source: &DeclarationSource,
    ) -> Result<DeclarationSource, PetError> {
        let workspace = self.workspace_for_source(project, &source.anchor.source)?;
        self.with_disposable(&workspace, |process| process.source_span(source))
    }

    /// Ask PET for the exact byte before the closing `End` of `modules`, or
    /// EOF for the compilation-unit top level. The process is disposable and
    /// retains no declaration catalogue.
    pub(crate) fn insertion_offset(
        &self,
        project: &Path,
        source: &Path,
        expected_digest: [u8; 32],
        modules: &[String],
        declaration: &DeclarationInfo,
    ) -> Result<usize, PetError> {
        let workspace = self.workspace_for_source(project, source)?;
        self.with_disposable(&workspace, |process| {
            process.insertion_offset(source, expected_digest, modules, declaration)
        })
    }

    /// Query exactly one Dune-selected source through PET's document API. The
    /// disposable process cannot retain that document after the call and the
    /// operation never walks unrelated workspace files.
    pub(crate) fn document_declarations(
        &self,
        dune: &crate::dune::Layout,
        file: &Path,
    ) -> Result<BTreeMap<DeclarationIdentity, DeclarationSource>, PetError> {
        let file = fs::canonicalize(file)
            .map_err(|_| PetError::Environment("declaration source is unavailable".into()))?;
        if !dune.contains_file(&file) {
            return Err(PetError::Environment(
                "source file is not selected by Dune".into(),
            ));
        }
        let library = dune
            .library(&file)
            .map_err(|error| PetError::Environment(error.message))?;
        let workspace = dune
            .prepare_pet_workspace(&file)
            .map_err(|error| PetError::Environment(error.message))?;
        self.with_disposable(&workspace, |process| {
            process.document_declarations(dune.workspace_root(), &file, library)
        })
    }

    /// Return explicit source assumptions from PET AST for the caller's
    /// Dune-resolved dependency files. A process is discarded after each
    /// source so this read-only read cannot accumulate PET documents.
    pub(crate) fn source_trust(
        &self,
        project: &Path,
        files: &[PathBuf],
    ) -> Result<PetSourceTrust, PetError> {
        if files.is_empty() {
            return Ok(PetSourceTrust {
                explicit_axioms: Vec::new(),
                admitted: Vec::new(),
            });
        }
        let dune = crate::dune::Layout::load(project, &[], self.timeout)
            .map_err(|error| PetError::Environment(error.message))?;
        let mut output = PetSourceTrust {
            explicit_axioms: Vec::new(),
            admitted: Vec::new(),
        };
        for file in files {
            let file = fs::canonicalize(file)
                .map_err(|_| PetError::Environment("assumption source is unavailable".into()))?;
            let workspace = dune
                .prepare_pet_workspace(&file)
                .map_err(|error| PetError::Environment(error.message))?;
            let file_trust =
                self.with_disposable(&workspace, |process| process.source_trust_file(&file))?;
            output.explicit_axioms.extend(file_trust.explicit_axioms);
            output.admitted.extend(file_trust.admitted);
        }
        Ok(output)
    }

    /// Return the exact Dune source-mapping root and generated project
    /// configuration used by a disposable PET for `source`.
    fn workspace_for_source(&self, project: &Path, source: &Path) -> Result<PathBuf, PetError> {
        let dune = crate::dune::Layout::load(project, &[], self.timeout)
            .map_err(|error| PetError::Environment(error.message))?;
        let source = fs::canonicalize(source)
            .map_err(|_| PetError::Environment("source is unavailable".into()))?;
        let workspace = dune
            .prepare_pet_workspace(&source)
            .map_err(|error| PetError::Environment(error.message))?;
        Ok(workspace)
    }

    pub(crate) fn detach(&self, project: &Path) {
        let Ok(project) = fs::canonicalize(project) else {
            return;
        };
        let pool = self
            .lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&project);
        if let Some(pool) = pool {
            pool.detach(&self.capacity);
        }
    }

    /// Retire cached handles after a successful source mutation without
    /// killing PET processes that may still be referenced by an already
    /// solved concurrent branch. The next lease rotates each process before
    /// it can serve a new step, so changed global definitions are never reused.
    pub(crate) fn invalidate_states(&self, project: &Path) {
        let Ok(project) = fs::canonicalize(project) else {
            return;
        };
        let pool = self
            .lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&project)
            .cloned();
        let Some(pool) = pool else {
            return;
        };
        let state = pool
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for lane in &state.lanes {
            lane.cached
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            lane.rotate_before_reuse.store(true, Ordering::Release);
        }
    }

    pub(crate) fn shutdown(&self) {
        let lanes = self.lanes.lock().map_or_else(
            |poisoned| std::mem::take(&mut *poisoned.into_inner()),
            |mut lanes| std::mem::take(&mut *lanes),
        );
        for pool in lanes.into_values() {
            pool.detach(&self.capacity);
        }
    }

    fn pool(&self, project: PathBuf) -> Arc<ProjectPool> {
        let mut lanes = self
            .lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lanes
            .entry(project)
            .or_insert_with(|| Arc::new(ProjectPool::new()))
            .clone()
    }

    /// Lease the root-affine lane. `Some(workspace_root)` permits a missing
    /// lane to be spawned at the exact Dune-selected root; `None` is a strict
    /// cache lookup and reports `Stale` instead of guessing a workspace.
    fn acquire(
        &self,
        pool: &Arc<ProjectPool>,
        workspace_root: Option<PathBuf>,
        affinity: String,
    ) -> Result<PetLease, PetError> {
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);
        loop {
            let mut state = pool
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.detached {
                return Err(PetError::ProcessFailure(
                    "project PET lane is detached".into(),
                ));
            }
            let mut dead = Vec::new();
            state.lanes.retain(|lane| {
                let dead_lane = !lane.busy.load(Ordering::Acquire) && lane.process.exited();
                if dead_lane {
                    dead.push(lane.process.clone());
                }
                !dead_lane
            });
            if !dead.is_empty() {
                drop(state);
                for _ in dead {
                    self.capacity.release();
                }
                pool.changed.notify_all();
                continue;
            }
            // Design note: equal roots single-flight; do not create a second
            // process whose document state could race the first replay.
            if state
                .lanes
                .iter()
                .any(|lane| lane.root_affinity == affinity && lane.busy.load(Ordering::Acquire))
            {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(PetError::Timeout);
                }
                let (next, _timeout) = pool
                    .changed
                    .wait_timeout(state, remaining.min(Duration::from_millis(100)))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                drop(next);
                continue;
            }
            if let Some(lane) = state.lanes.iter().find(|lane| {
                lane.root_affinity == affinity
                    && !lane.busy.load(Ordering::Acquire)
                    && lane
                        .busy
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
            }) {
                return Ok(PetLease {
                    pool: Arc::clone(pool),
                    lane: Arc::clone(lane),
                    capacity: Arc::clone(&self.capacity),
                });
            }
            if state.spawning {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(PetError::Timeout);
                }
                let (next, _timeout) = pool
                    .changed
                    .wait_timeout(state, remaining.min(Duration::from_millis(100)))
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                drop(next);
                continue;
            }
            // Cache-only callers must never manufacture a PET with the
            // project root: the exact PET workspace is a Dune result known
            // only after document preparation. Missing cached ownership is a
            // stale handle and is recovered by replay through that path.
            let Some(workspace_root) = workspace_root.as_ref() else {
                return Err(PetError::Stale);
            };
            state.spawning = true;
            drop(state);
            let remaining = deadline.saturating_duration_since(Instant::now());
            let reserved = self.capacity.reserve_with_reaper(remaining, || {
                self.reap_dead_processes();
                self.evict_idle_lane()
            });
            if let Err(error) = reserved {
                let mut state = pool
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.spawning = false;
                pool.changed.notify_all();
                return Err(error);
            }
            let spawned = PetProcess::spawn(workspace_root.clone(), self.timeout, &self.pet_binary);
            let mut state = pool
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.spawning = false;
            let result = match spawned {
                Ok(process) if !state.detached => {
                    let lane = Arc::new(ProjectLane {
                        process,
                        root_affinity: affinity,
                        busy: AtomicBool::new(true),
                        cached: Mutex::new(None),
                        document_key: Mutex::new(None),
                        rotate_before_reuse: AtomicBool::new(false),
                    });
                    let lease = PetLease {
                        pool: Arc::clone(pool),
                        lane: Arc::clone(&lane),
                        capacity: Arc::clone(&self.capacity),
                    };
                    state.lanes.push(lane);
                    Ok(lease)
                }
                Ok(process) => {
                    process.terminate();
                    self.capacity.release();
                    Err(PetError::ProcessFailure(
                        "project PET lane is detached".into(),
                    ))
                }
                Err(error) => {
                    self.capacity.release();
                    Err(error)
                }
            };
            pool.changed.notify_all();
            return result;
        }
    }

    fn invalidate(&self, pool: &Arc<ProjectPool>, lane: &Arc<ProjectLane>) {
        if let Some(process) = pool.remove_lane(lane) {
            process.terminate();
            self.capacity.release();
        }
        self.capacity.wake();
    }

    fn disposable(&self, workspace_root: PathBuf) -> Result<DisposablePet, PetError> {
        self.capacity.reserve_with_reaper(self.timeout, || {
            self.reap_dead_processes();
            self.evict_idle_lane()
        })?;
        match PetProcess::spawn(workspace_root, self.timeout, &self.pet_binary) {
            Ok(process) => Ok(DisposablePet {
                process,
                capacity: Arc::clone(&self.capacity),
            }),
            Err(error) => {
                self.capacity.release();
                Err(error)
            }
        }
    }

    fn reap_dead_processes(&self) {
        let pools = self
            .lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for pool in pools {
            for _ in 0..pool.reap_dead_idle() {
                self.capacity.release();
            }
        }
    }

    fn evict_idle_lane(&self) -> bool {
        let pools = self
            .lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for pool in pools {
            if let Some(process) = pool.evict_idle() {
                process.terminate();
                self.capacity.release();
                self.capacity.wake();
                return true;
            }
        }
        false
    }
}

impl Drop for PetRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}
