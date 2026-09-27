//! Thin orchestration over project state, PET, Dune, trace forest, and writeback.

use super::*;
use crate::types::{DeclarationInstance, PetAnchor, PetRange, ProjectState, ProofAttempt};
use std::{collections::BTreeMap, sync::Arc};

/// The semantic engine.  The forest stores only logical roots and canonical
/// actions; PET state is deliberately not represented here.
pub struct Engine {
    pub(crate) config: EngineConfig,
    pub(crate) pet: pet::PetRuntime,
    /// One authoritative state object per canonical Dune workspace.  The
    /// trace forest is owned by this object; no global cross-project forest
    /// exists.
    pub(crate) projects: Mutex<BTreeMap<PathBuf, Arc<Mutex<ProjectState>>>>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Design note: native children are reaped before project-owned runtime state is dropped.
        self.pet.shutdown();
    }
}

impl Engine {
    /// Return the canonical project path and its project-owned operation gate.
    /// The project map is the sole registry; no parallel attachment table or
    /// filesystem lock directory exists.
    fn project_access(&self, project: &Path) -> Result<(PathBuf, Arc<std::sync::RwLock<()>>)> {
        let root = std::fs::canonicalize(project).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "project path is unavailable",
            )
        })?;
        let state = self.project_state(&root)?;
        let state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok((state.root.clone(), Arc::clone(&state.gate)))
    }

    pub fn new(config: EngineConfig) -> Result<Self> {
        if !(1..=64).contains(&config.max_pet_processes) {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "max PET processes must be between 1 and 64",
            ));
        }
        let pet = pet::PetRuntime::new(
            config.operation_timeout,
            config.max_pet_processes,
            config.runtime_cache_bytes,
        )
        .map_err(|error| Error::new(ErrorKind::InvalidConfiguration, error.to_string()))?;
        Ok(Self {
            config,
            pet,
            projects: Mutex::new(BTreeMap::new()),
        })
    }

    /// Attach a caller path to its canonical Dune workspace without starting
    /// PET or indexing declarations.  Success guarantees that subsequent
    /// connection-scoped operations have one initialized ProjectState.
    pub fn attach(&self, project: &Path) -> Result<PathBuf> {
        let attached = std::fs::canonicalize(project).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "project path is unavailable",
            )
        })?;
        // Every explicit start revalidates Dune. Internal calls use the
        // canonical returned root and may take `project_state`'s fast path.
        let root = dune::dune_workspace_root(&attached, self.config.operation_timeout)?;
        self.project_state_for_root(root.clone())?;
        Ok(root)
    }

    /// Returns the one state object for a canonical workspace, creating its
    /// project-local trace forest on first access. Temporary Dune views are
    /// queried on demand; project state retains only touched
    /// declarations and trace roots.
    pub(crate) fn project_state(&self, project: &Path) -> Result<Arc<Mutex<ProjectState>>> {
        let attached = std::fs::canonicalize(project).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "project path is unavailable",
            )
        })?;
        // The MCP selection stores the canonical root returned by `attach`.
        // Avoid rediscovering that same Dune workspace on every later call;
        // only a previously unseen path needs Dune identity resolution.
        if let Some(state) = self
            .projects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&attached)
            .cloned()
        {
            return Ok(state);
        }
        // Dune's workspace identity is the project identity. Attaching two
        // subdirectories of one workspace must resolve to one ProjectState.
        let project = dune::dune_workspace_root(&attached, self.config.operation_timeout)?;
        self.project_state_for_root(project)
    }

    /// Return or initialize the sole state object for an already
    /// Dune-canonicalized workspace root. No Dune command is issued here.
    fn project_state_for_root(&self, project: PathBuf) -> Result<Arc<Mutex<ProjectState>>> {
        let mut projects = self
            .projects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = projects.get(&project) {
            return Ok(Arc::clone(state));
        }
        let traces = TraceForest::new(ForestConfig::new(
            self.config.trace_memory_bytes,
            &self.config.state_parent,
        ))
        .map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "trace state directory cannot be initialized",
            )
        })?;
        let state = Arc::new(Mutex::new(ProjectState {
            root: project.clone(),
            gate: Arc::new(std::sync::RwLock::new(())),
            traces: Arc::new(traces),
            touched: BTreeMap::new(),
        }));
        projects.insert(project, Arc::clone(&state));
        Ok(state)
    }

    /// Return the trace forest owned by one canonical project state.
    pub(crate) fn traces_for_project(
        &self,
        project: &Path,
    ) -> Result<Arc<TraceForest<DeclarationSource, CanonicalTactic>>> {
        Ok(Arc::clone(
            &self
                .project_state(project)?
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .traces,
        ))
    }

    pub(crate) fn traces_for_attempt(
        &self,
        attempt: AttemptId,
    ) -> Result<Arc<TraceForest<DeclarationSource, CanonicalTactic>>> {
        let project = self.attempt_project(attempt)?;
        self.traces_for_project(&project)
    }

    /// Return Dune-selected source files as workspace-relative IDs. This is a
    /// Dune operation and does not start PET or parse source files.
    pub fn list_files(&self, project: &Path) -> Result<Vec<FileId>> {
        let (project, gate) = self.project_access(project)?;
        let _read = gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dune = dune::Layout::load(
            &project,
            &self.owned_path(&project),
            self.config.operation_timeout,
        )?;
        dune.files()
            .into_iter()
            .map(|path| {
                FileId::from_path(&project, &path)
                    .map_err(|message| Error::new(ErrorKind::InvalidConfiguration, message))
            })
            .collect()
    }

    /// Return PET document declarations from exactly one Dune-selected source.
    pub fn list_decls(&self, project: &Path, file: &FileId) -> Result<Vec<DeclarationInfo>> {
        let (project, gate) = self.project_access(project)?;
        let _read = gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = project.join(&file.0);
        let dune = dune::Layout::load(
            &project,
            &self.owned_path(&project),
            self.config.operation_timeout,
        )?;
        if !dune.contains_file(&source) {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune has not selected the requested source file",
            ));
        }
        self.pet
            .document_declarations(&dune, &source)
            .map_err(|error| self.pet_error(error))
            .map(|declarations| {
                declarations
                    .into_values()
                    .map(|source| source.info)
                    .collect()
            })
    }

    /// Opens one PET-parsed unfinished declaration. Existing declarations are
    /// checked by PET and Dune/Rocq before
    /// returning a completed state; no source-text status is trusted.
    pub fn open(&self, project: &Path, identity: DeclarationIdentity) -> Result<ProofState> {
        self.open_declaration(project, identity)
    }

    /// Open a declaration returned by `list_decls`. PET resolves only the
    /// declaration's source file; unrelated workspace files are untouched.
    pub fn open_declaration(
        &self,
        project: &Path,
        identity: DeclarationIdentity,
    ) -> Result<ProofState> {
        validate_identity(&identity)?;
        let project = self.load_declaration(project, &identity)?;
        self.open_inner_loaded(&project, identity)
    }

    /// Load one declaration from its authoritative PET source and remember it
    /// as touched. No workspace-wide declaration index is constructed.
    fn load_declaration(&self, project: &Path, identity: &DeclarationIdentity) -> Result<PathBuf> {
        validate_identity(identity)?;
        let (project, gate) = self.project_access(project)?;
        let _read = gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source_path = project.join(&identity.file.0);
        let dune = dune::Layout::load(
            &project,
            &self.owned_path(&project),
            self.config.operation_timeout,
        )?;
        if !dune.contains_file(&source_path) {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune has not selected the declaration source",
            ));
        }
        let sources = self
            .pet
            .document_declarations(&dune, &source_path)
            .map_err(|error| self.pet_error(error))?;
        let source = sources.get(identity).cloned().ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                format!("declaration '{}' was not found", format_name(identity)),
            )
        })?;
        let project_state = self.project_state(&project)?;
        let mut state = project_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.touched.entry(identity.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(DeclarationInstance {
                    source,
                    attempts: BTreeMap::new(),
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                // Design note: a successful writeback changes the file digest
                // while older trace roots remain valid only for their old
                // snapshot. Refresh the declaration anchor for a new open,
                // but retain old attempt handles so a subsequent step can
                // deterministically report DeclarationChanged.
                if entry.get().source.anchor.digest != source.anchor.digest {
                    entry.get_mut().source = source;
                }
            }
        }
        Ok(project)
    }

    /// Reconstruct the PET source anchor from the declaration-owned state.
    /// Query resolution uses this single identity-keyed record and never a
    /// second source index.
    pub(crate) fn source_for_identity(
        &self,
        project: &Path,
        identity: &DeclarationIdentity,
    ) -> Result<DeclarationSource> {
        let state = self.project_state(project)?;
        let state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let declaration = state.touched.get(identity).ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                "declaration has no source document yet",
            )
        })?;
        let anchor = &declaration.source.anchor;
        if anchor.header.start >= anchor.header.end
            || !anchor.source.is_file()
            || std::fs::read(&anchor.source)
                .ok()
                .is_none_or(|bytes| <[u8; 32]>::from(Sha256::digest(&bytes)) != anchor.digest)
        {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "declaration source anchor is unavailable",
            ));
        }
        Ok(declaration.source.clone())
    }

    fn open_inner_loaded(
        &self,
        project: &Path,
        identity: DeclarationIdentity,
    ) -> Result<ProofState> {
        validate_identity(&identity)?;
        let (project, project_gate) = self.project_access(project)?;
        let _gate = project_gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let info = {
            let state = self.project_state(&project)?;
            let state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let declaration = state.touched.get(&identity).ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    format!("declaration '{}' was not found", format_name(&identity)),
                )
            })?;
            DeclarationInfo {
                identity: declaration.source.info.identity.clone(),
                kind: declaration.source.info.kind,
                statement: declaration.source.info.statement.clone(),
            }
        };

        let source = self.source_for_identity(&project, &identity)?;
        let source = self
            .pet
            .source_span(&project, &source)
            .map_err(|error| self.pet_error(error))?;
        let proof_completed = self
            .pet
            .source_proof_completed(&project, &source)
            .map_err(|error| self.pet_error(error))?;
        if proof_completed {
            // Design note: PET recovers some invalid Qed scripts as self-axioms.
            // A source terminator is not a proof result: native compilation
            // must succeed before PET's assumption view can be trusted.
            let target = source.anchor.source.strip_prefix(&project).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "source target escapes project",
                )
            })?;
            writeback::native_build(
                &project,
                target,
                self.config.close_timeout,
                self.config.operation_timeout,
            )?;
            let assumptions = self
                .pet
                .source_assumptions(
                    &project,
                    &source.anchor.source,
                    source
                        .anchor
                        .declaration
                        .as_ref()
                        .ok_or_else(|| {
                            Error::new(
                                ErrorKind::DeclarationChanged,
                                "PET declaration range is unavailable",
                            )
                        })?
                        .end,
                    source.anchor.digest,
                    Some((
                        source.info.identity.constant().unwrap_or_default(),
                        source.info.kind,
                        source.anchor.header.end,
                    )),
                    identity.constant().unwrap_or_default(),
                )
                .map_err(|error| self.pet_error(error))?;
            writeback::audit_existing_source(
                self,
                &project,
                &source,
                &assumptions,
                self.config.operation_timeout,
                self.config.close_timeout,
            )?;
            return Ok(state(None, info, ProofLifecycle::Completed));
        }
        let open = source.clone();
        let key = root_key(&open)?;
        let traces = self.traces_for_project(&project)?;
        let cursor = traces
            .open(key, || Ok::<_, Error>(open.clone()))
            .map_err(trace_call_error)?;
        self.remember_attempt(cursor, &project);
        // Design note: opening is an interactive operation, so its goals must
        // come from PET rather than a topology-only placeholder.
        let native = self
            .pet
            .restore_state(&project, &open, None, &[])
            .map_err(|error| self.pet_error(error))?;
        self.remember_pet_state(AttemptId(cursor), &native, 0);
        Ok(self.state_from_pet(AttemptId(cursor), &open, &native, ProofLifecycle::Open))
    }

    /// Declares a logical theorem without editing source. Dune selects the
    /// insertion file and PET validates the declaration by running its header
    /// from the target file's end state; the anchor stores immutable bytes.
    pub fn declare(
        &self,
        project: &Path,
        kind: DeclarationKind,
        identity: DeclarationIdentity,
        statement: String,
    ) -> Result<ProofState> {
        self.declare_inner(project, kind, identity, statement)
    }

    /// Discards every unpublished trace root for one exactly identified
    /// declaration. Source is never edited.
    ///
    /// All source-version roots are retired before their attempt records are
    /// removed. If retirement fails, the records remain addressable so the
    /// caller never loses the only handle to an unretired trace.
    pub fn abandon(&self, project: &Path, identity: DeclarationIdentity) -> Result<String> {
        validate_identity(&identity)?;
        let (project, project_gate) = self.project_access(project)?;
        let traces = self.traces_for_project(&project)?;
        let _gate = project_gate
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = self.project_state(&project)?;
        let state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cursors = state
            .touched
            .get(&identity)
            .map(|declaration| {
                declaration
                    .attempts
                    .keys()
                    .map(|attempt| attempt.0)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        drop(state);
        if cursors.is_empty() {
            return Err(Error::new(
                ErrorKind::NotFound,
                "no active unpublished proof has that declaration",
            ));
        }
        // Design note: one DeclarationId can own simultaneous attempts rooted
        // in different source digests. Closing one cursor retires only that
        // cursor's root family, so visit every remembered cursor; repeated
        // cursors from one family harmlessly report AlreadyRetired/UnknownCursor
        // after the first close and need no wrapper-side root index.
        for cursor in &cursors {
            match traces.close(*cursor, |_| Ok::<(), Error>(())) {
                Ok(_)
                | Err(trace_forest::CallError::Forest(trace_forest::Error::UnknownCursor)) => {}
                Err(error) => return Err(trace_call_error(error)),
            }
        }
        let state = self.project_state(&project)?;
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(declaration) = state.touched.get_mut(&identity) {
            for cursor in cursors {
                declaration.attempts.remove(&AttemptId(cursor));
            }
        }
        // A declaration without a PET source range exists only because this
        // process declared it interactively. Once its trace is abandoned it
        // has no remaining owner and must not shadow a later declaration.
        if state.touched.get(&identity).is_some_and(|declaration| {
            declaration.source.anchor.header.start >= declaration.source.anchor.header.end
        }) {
            state.touched.remove(&identity);
        }
        self.pet.invalidate_states(&project);
        Ok(format_name(&identity))
    }
    fn declare_inner(
        &self,
        project: &Path,
        kind: DeclarationKind,
        identity: DeclarationIdentity,
        statement: String,
    ) -> Result<ProofState> {
        validate_new_declaration(&identity, &statement)?;
        let (project, project_gate) = self.project_access(project)?;
        let _gate = project_gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .project_state(&project)?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .touched
            .contains_key(&identity)
        {
            return Err(Error::new(
                ErrorKind::InvalidDeclaration,
                "declaration identity is already present",
            ));
        }

        let layout = dune::Layout::load(&project, &[], self.config.operation_timeout)?;
        let path = project.join(&identity.file.0);
        if !path.starts_with(&project) {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "declaration target is outside the project",
            ));
        }
        if !layout.files().contains(&path) {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune has not selected the declaration target source",
            ));
        }
        let source = std::fs::read(&path).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "declaration target is unavailable",
            )
        })?;

        let source_digest = Sha256::digest(&source).into();
        let normalized_statement = new_declaration_header(kind, &identity, &statement)?;
        // Design note: pass one immutable declaration descriptor through the
        // PET placement check so identity, kind, and header cannot diverge
        // between validation and the trace root created below.
        let declaration_info = DeclarationInfo {
            identity: identity.clone(),
            kind,
            statement: normalized_statement.clone(),
        };
        let library = layout.library(&path)?.clone();
        let identity_prefix = &identity.qualified_path[..identity.qualified_path.len() - 1];
        if !identity_prefix.starts_with(&library.0) {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "logical library is not selected by Dune",
            ));
        }
        let modules = &identity_prefix[library.0.len()..];
        let insertion = self
            .pet
            .insertion_offset(&project, &path, source_digest, modules, &declaration_info)
            .map_err(|error| self.pet_error(error))?;
        let open = DeclarationSource {
            info: declaration_info,
            library,
            anchor: PetAnchor {
                source: path,
                digest: source_digest,
                // A zero-width PET-derived range denotes a new declaration's
                // exact insertion point; it is not a sentinel for file zero.
                header: PetRange {
                    start: insertion,
                    end: insertion,
                },
                declaration: None,
            },
        };
        let traces = self.traces_for_project(&project)?;
        let cursor = traces
            .open(root_key(&open)?, || Ok::<_, Error>(open.clone()))
            .map_err(trace_call_error)?;
        #[cfg(all(feature = "fault-injection", unix))]
        declare_race_barrier()?;
        self.remember_attempt(cursor, &project);
        let native = self
            .pet
            .restore_state(&project, &open, None, &[])
            .map_err(|error| self.pet_error(error))?;
        self.remember_pet_state(AttemptId(cursor), &native, 0);
        Ok(self.state_from_pet(AttemptId(cursor), &open, &native, ProofLifecycle::Open))
    }

    /// Register a TraceForest cursor under its declaration-owned project state.
    /// PET evaluation and trace publication happen before this bookkeeping, so
    /// a rejected fragment is never registered as an attempt.
    pub(crate) fn remember_attempt(&self, cursor: CursorId, project: &Path) {
        let traces = self
            .traces_for_project(project)
            .expect("project state exists before an attempt is remembered");
        if let Ok(view) = traces.inspect(cursor)
            && let Ok(state) = self.project_state(project)
        {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let root = view.root().clone();
            let declaration = state
                .touched
                .entry(root.info.identity.clone())
                .or_insert_with(|| DeclarationInstance {
                    source: root.clone(),
                    attempts: BTreeMap::new(),
                });
            declaration
                .attempts
                .insert(AttemptId(cursor), ProofAttempt { pet_state: None });
        }
        // Design note: the declaration map is the sole owner of attempts.
        // Project lookup is derived by walking those authoritative records;
        // no second cursor-to-project state table is maintained.
    }
    pub(crate) fn attempt_project(&self, attempt: AttemptId) -> Result<PathBuf> {
        let projects = self
            .projects
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (project, state) in projects.iter() {
            let state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state
                .touched
                .values()
                .any(|declaration| declaration.attempts.contains_key(&attempt))
            {
                return Ok(project.clone());
            }
        }
        Err(Error::new(
            ErrorKind::NotFound,
            "proof attempt is no longer open",
        ))
    }

    /// Cache PET's opaque proof handle beside the owning declaration attempt.
    /// The handle is only a valid fast path while its process epoch matches;
    /// PET replay remains the recovery path after eviction or restart.
    pub(crate) fn remember_pet_state(
        &self,
        attempt: AttemptId,
        native: &pet::PetState,
        prefix_len: usize,
    ) {
        let Ok(project) = self.attempt_project(attempt) else {
            return;
        };
        let Ok(state) = self.project_state(&project) else {
            return;
        };
        let mut state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(declaration) = state
            .touched
            .values_mut()
            .find(|declaration| declaration.attempts.contains_key(&attempt))
            && let Some(record) = declaration.attempts.get_mut(&attempt)
        {
            record.pet_state = Some(crate::types::PetProofState {
                instance_epoch: native.instance_epoch,
                state: native.st,
                prefix_len,
            });
        }
    }

    /// Resolve an attempt's live PET handle, replaying its trace only when the
    /// handle was evicted, invalidated, or belongs to a different prefix.
    fn pet_state_for_attempt(
        &self,
        project: &Path,
        attempt: AttemptId,
        root: &DeclarationSource,
        actions: &[CanonicalTactic],
    ) -> Result<pet::PetState> {
        // Design note: this is the single gateway from an engine attempt to
        // PET state. Validate both the immutable source anchor and Dune's
        // current source selection here so no cached-handle caller can bypass
        // the project environment contract.
        self.validate_attempt_environment(project, root)?;
        let handle = self
            .project_state(project)?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .touched
            .values()
            .find_map(|declaration| declaration.attempts.get(&attempt))
            .and_then(|record| record.pet_state)
            .filter(|handle| handle.prefix_len == actions.len());
        let preferred = handle.map(|handle| (handle.instance_epoch, handle.state));
        let native = self
            .pet
            .restore_state(project, root, preferred, actions)
            .map_err(|error| self.pet_error(error))?;
        self.remember_pet_state(attempt, &native, actions.len());
        Ok(native)
    }

    /// Verify that an immutable trace root still names the exact source bytes
    /// from which PET created it.  Every operation that resumes an attempt
    /// uses this one check before consulting an opaque PET state.
    fn validate_attempt_source(&self, project: &Path, root: &DeclarationSource) -> Result<()> {
        let bytes = std::fs::read(&root.anchor.source).map_err(|_| {
            Error::new(
                ErrorKind::DeclarationChanged,
                "declaration source changed while proof was open",
            )
        })?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != root.anchor.digest {
            self.pet.detach(project);
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "declaration source changed while proof was open",
            ));
        }
        Ok(())
    }

    /// Validate Dune's current source-selection contract before a new
    /// operation consumes a cached PET state. Source bytes are checked first
    /// so edits/deletions retain the precise `declaration_changed` diagnostic.
    fn validate_attempt_environment(&self, project: &Path, root: &DeclarationSource) -> Result<()> {
        self.validate_attempt_source(project, root)?;
        let layout = dune::Layout::load(
            project,
            &self.owned_path(project),
            self.config.operation_timeout,
        )?;
        if !layout.contains_file(&root.anchor.source) {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune has not selected the declaration source",
            ));
        }
        Ok(())
    }

    pub(crate) fn pet_error(&self, error: pet::PetError) -> Error {
        let message = error.to_string();
        match error {
            pet::PetError::Timeout | pet::PetError::OperationTimeout => {
                Error::new(ErrorKind::ProofTimeout, message)
            }
            pet::PetError::InvalidDeclaration(_) => {
                Error::new(ErrorKind::InvalidDeclaration, message)
            }
            pet::PetError::UnsafeProofCommand(_) => Error::new(ErrorKind::InvalidRequest, message),
            pet::PetError::Remote { .. } => Error::new(ErrorKind::ProofStepFailed, message),
            pet::PetError::Stale | pet::PetError::Protocol(_) | pet::PetError::OutputOverflow => {
                Error::new(ErrorKind::InvalidConfiguration, message)
            }
            pet::PetError::Invalid(ref detail)
                if detail.contains("interface changed")
                    || detail.contains("source changed")
                    || detail.contains("disappeared") =>
            {
                Error::new(ErrorKind::DeclarationChanged, message)
            }
            pet::PetError::Invalid(ref detail)
                if detail.contains("requested declaration module context does not exist") =>
            {
                Error::new(ErrorKind::InvalidDeclaration, message)
            }
            pet::PetError::Invalid(_) => Error::new(ErrorKind::InvalidConfiguration, message),
            pet::PetError::Environment(_) => Error::new(ErrorKind::InvalidConfiguration, message),
            // A process failure reaches this boundary only after the runtime's
            // fresh-process replay has failed. It is an operation failure, not
            // a new public catch-all class.
            pet::PetError::ProcessFailure(_) => Error::new(ErrorKind::ProofTimeout, message),
        }
    }
    pub(crate) fn state_from_pet(
        &self,
        attempt: AttemptId,
        root: &DeclarationSource,
        native: &pet::PetState,
        lifecycle: ProofLifecycle,
    ) -> ProofState {
        let goals = &native.goals;
        let info = DeclarationInfo {
            identity: root.info.identity.clone(),
            kind: root.info.kind,
            statement: root.info.statement.clone(),
        };
        let text = render_pet_goals(goals);
        ProofState {
            attempt: Some(attempt),
            theorem: info,
            lifecycle,
            focused_goals: goals.focused.len(),
            unfocused_goals: goals.unfocused.len(),
            shelved_goals: goals.shelved.len(),
            given_up_goals: goals.given_up.len(),
            goals: text,
        }
    }

    pub(crate) fn owned_path(&self, root: &Path) -> Vec<PathBuf> {
        let state = self.config.state_parent.as_path();
        if state.starts_with(root) && state != root {
            vec![state.to_owned()]
        } else {
            Vec::new()
        }
    }
}

/// Test-only barrier between source anchoring and PET document construction.
/// The external E2E fixture mutates its disposable source after receiving the
/// byte and acknowledges completion before replay resumes. Normal builds do
/// not contain this branch or expose any test command to MCP users.
#[cfg(all(feature = "fault-injection", unix))]
fn declare_race_barrier() -> Result<()> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let Ok(socket) = std::env::var("ROCQ_ENGINE_DECLARE_RACE_SOCKET") else {
        return Ok(());
    };
    let mut stream = UnixStream::connect(socket).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    let timeout = Some(Duration::from_secs(5));
    stream.set_read_timeout(timeout).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    stream.set_write_timeout(timeout).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    stream.write_all(&[1]).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    let mut acknowledged = [0u8; 1];
    stream.read_exact(&mut acknowledged).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        )
    })?;
    if acknowledged != [1] {
        return Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "declare race fixture unavailable",
        ));
    }
    Ok(())
}

/// Render only PET's structured semantic fields; this is presentation, never
/// goal inference or console-output parsing.
fn render_pet_goals(goals: &pet::PetGoals) -> String {
    let mut output = String::new();
    for (label, collection) in [
        ("focused", &goals.focused),
        ("unfocused", &goals.unfocused),
        ("shelved", &goals.shelved),
        ("given_up", &goals.given_up),
    ] {
        output.push_str(label);
        output.push_str(":\n");
        for goal in collection {
            for hypothesis in &goal.hypotheses {
                output.push_str("  ");
                output.push_str(&hypothesis.names.join(" "));
                if let Some(definition) = &hypothesis.definition {
                    output.push_str(" := ");
                    output.push_str(definition);
                }
                output.push_str(" : ");
                output.push_str(&hypothesis.ty);
                output.push('\n');
            }
            output.push_str("  ============================\n  ");
            output.push_str(&goal.ty);
            output.push('\n');
        }
    }
    output
}

fn state(
    attempt: Option<AttemptId>,
    theorem: DeclarationInfo,
    lifecycle: ProofLifecycle,
) -> ProofState {
    ProofState {
        attempt,
        theorem,
        lifecycle,
        focused_goals: 0,
        unfocused_goals: 0,
        shelved_goals: 0,
        given_up_goals: 0,
        goals: String::new(),
    }
}

/// Validate the syntactic components of a declaration identity before any
/// Dune placement or PET operation.  MCP uses this same engine-owned contract
/// to distinguish malformed names from valid names in the wrong compilation
/// unit; no transport layer reimplements identifier rules.
pub fn validate_identity(identity: &DeclarationIdentity) -> Result<()> {
    let relative_file = std::path::Path::new(&identity.file.0);
    if identity.file.0.is_empty()
        || relative_file.is_absolute()
        || relative_file
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        || identity.qualified_path.is_empty()
        || !identity
            .qualified_path
            .iter()
            .all(|part| valid_identifier(part))
    {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration identity is invalid",
        ));
    }
    Ok(())
}

pub(crate) fn trace_error(error: trace_forest::Error) -> Error {
    match error {
        trace_forest::Error::UnknownCursor
        | trace_forest::Error::Retired
        | trace_forest::Error::PayloadCodec
        | trace_forest::Error::CorruptSpill => {
            Error::new(ErrorKind::NotFound, "proof attempt is no longer open")
        }
        trace_forest::Error::Closing | trace_forest::Error::ConcurrentPreparationFailed => {
            Error::new(
                ErrorKind::ProofTimeout,
                "proof transition did not settle in time",
            )
        }
        trace_forest::Error::PrefixOutOfRange => Error::new(
            ErrorKind::InvalidRequest,
            "trace prefix is outside the selected proof branch",
        ),
        trace_forest::Error::InvalidKey | trace_forest::Error::SpillUnavailable => Error::new(
            ErrorKind::InvalidConfiguration,
            "trace state storage is unavailable",
        ),
    }
}

fn trace_call_error(error: trace_forest::CallError<Error>) -> Error {
    match error {
        trace_forest::CallError::Callback(error) => error,
        trace_forest::CallError::Forest(error) => trace_error(error),
    }
}

fn validate_new_declaration(identity: &DeclarationIdentity, statement: &str) -> Result<()> {
    validate_identity(identity)?;
    if statement.trim().is_empty() || statement.len() > 1024 * 1024 {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration statement is empty or oversized",
        ));
    }
    // Design note: statement parsing and identity validation belong to PET.
    // This boundary only bounds the user payload; PET immediately parses the
    // constructed declaration when the trace root is opened.
    Ok(())
}

fn new_declaration_header(
    kind: DeclarationKind,
    identity: &DeclarationIdentity,
    statement: &str,
) -> Result<String> {
    // Design note: preserve the user's Rocq syntax byte-for-byte inside the
    // sentence; collapsing whitespace would also rewrite string literals.
    let body = statement.trim();
    if body.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration statement is empty",
        ));
    }
    let keyword = declaration_kind_keyword(kind);
    let prefix = format!(
        "{keyword} {}",
        identity.constant().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidDeclaration,
                "declaration has no leaf name",
            )
        })?
    );
    if body
        .split_whitespace()
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case(keyword))
    {
        Ok(body.trim_end_matches('.').trim().to_owned())
    } else {
        Ok(format!("{prefix} : {body}"))
    }
}

fn declaration_kind_keyword(kind: DeclarationKind) -> &'static str {
    match kind {
        DeclarationKind::Theorem => "Theorem",
        DeclarationKind::Lemma => "Lemma",
        DeclarationKind::Fact => "Fact",
        DeclarationKind::Remark => "Remark",
        DeclarationKind::Corollary => "Corollary",
        DeclarationKind::Proposition => "Proposition",
        DeclarationKind::Definition => "Definition",
    }
}

fn root_key(declaration: &DeclarationSource) -> Result<RootKey> {
    let mut digest = Sha256::new();
    let file = declaration.info.identity.file.0.as_bytes();
    // Design note: FileId is part of DeclarationId even when Dune currently
    // makes the qualified name globally unique. Omitting it aliases distinct
    // declaration identities whenever their path and source digest coincide.
    digest.update((file.len() as u64).to_le_bytes());
    digest.update(file);
    for part in &declaration.info.identity.qualified_path {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    // A source edit creates a new immutable trace root. Reusing an identity-
    // only root would resurrect stale PET anchors after an explicit reopen.
    digest.update(declaration.anchor.digest);
    Ok(RootKey::from_digest(digest.finalize().into()))
}

pub(crate) fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64 * 1024
        && value
            .chars()
            .all(|x| x == '_' || x == '\'' || x.is_alphanumeric())
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64 * 1024
        || name.split('.').any(|part| !valid_identifier(part))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "query name is invalid",
        ));
    }
    Ok(())
}

pub(crate) fn validate_native_fragment(fragment: &str) -> Result<()> {
    if fragment.trim().is_empty()
        || fragment.len() > 1024 * 1024
        || fragment.chars().any(char::is_control)
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "query expression is invalid",
        ));
    }
    Ok(())
}

// Solved-branch to synchronous writeback orchestration.
impl Engine {
    /// Publish a PET-completed branch while the trace-forest close callback
    /// still owns the branch. A writeback failure leaves the attempt open and
    /// returns the concrete failure to the caller; there is no pending or
    /// recovered publication state.
    pub(crate) fn close_solved(
        &self,
        attempt: AttemptId,
        native: &pet::PetState,
    ) -> Result<Option<(ProofState, Option<Error>)>> {
        if !(native.proof_finished && native.goals.all_clear()) {
            return Ok(None);
        }
        let project = self.attempt_project(attempt)?;
        let traces = self.traces_for_project(&project)?;
        let (project, project_gate) = self.project_access(&project)?;
        let _gate = project_gate
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let selected = traces.inspect(attempt.0).map_err(trace_error)?;
        let root = selected.root().clone();
        let mut published = None;
        let outcome = traces.close(attempt.0, |view| {
            let state = writeback::publish(self, &project, view.root(), view.actions(), native)?;
            published = Some(state);
            Ok::<(), Error>(())
        });
        match outcome {
            Ok(trace_forest::CloseOutcome::Closed)
            | Ok(trace_forest::CloseOutcome::AlreadyRetired) => {}
            Err(trace_forest::CallError::Callback(error)) => {
                // The callback is transactional: on failure TraceForest keeps
                // the branch live. A source-CAS failure is the exception to
                // actionability: its immutable root no longer matches the
                // project, so it must not escape as a selectable checkpoint.
                let mut state = self.state_from_pet(attempt, &root, native, ProofLifecycle::Open);
                if error.kind == ErrorKind::DeclarationChanged {
                    state.attempt = None;
                }
                return Ok(Some((state, Some(error))));
            }
            Err(trace_forest::CallError::Forest(error)) => return Err(trace_error(error)),
        }
        let state = published.ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "writeback completed without a proof state",
            )
        })?;
        if let Ok(project_state) = self.project_state(&project) {
            let mut project_state = project_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(declaration) = project_state.touched.get_mut(&root.info.identity) {
                declaration.attempts.clear();
            }
        }
        self.pet.invalidate_states(&project);
        Ok(Some((state, None)))
    }
}

/// Split and bound every requested fragment before any PET state is touched.
/// Rocq parsing and tactic validity remain PET responsibilities; this helper
/// only establishes request framing and TraceForest key invariants.
fn validated_attempts(attempts: &[String]) -> Result<Vec<Vec<CanonicalTactic>>> {
    if !(1..=20).contains(&attempts.len()) {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "attempt count must be between 1 and 20",
        ));
    }
    attempts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let ranges = pet::sentence_ranges(text).map_err(|_| {
                Error::new(
                    ErrorKind::InvalidRequest,
                    format!("attempt {index} cannot be split into Rocq sentences"),
                )
            })?;
            if ranges.is_empty() {
                return Err(Error::new(
                    ErrorKind::InvalidRequest,
                    format!("attempt {index} is empty"),
                ));
            }
            ranges
                .into_iter()
                .map(|range| {
                    let tactic = pet::canonical_tactic(&text[range])?;
                    ActionKey::new(tactic.0.as_bytes()).map_err(|_| {
                        Error::new(ErrorKind::InvalidRequest, "proof sentence is too large")
                    })?;
                    Ok(tactic)
                })
                .collect()
        })
        .collect()
}

// Interactive trace/PET orchestration.
impl Engine {
    /// Evaluate ordered proof fragments from one immutable base and commit the
    /// first fragment whose every sentence PET accepts.
    ///
    /// Inputs are validated before PET runs. Rejected fragments never append a
    /// trace prefix, and a winning multi-sentence fragment is selected only
    /// after its final sentence succeeds. The already evaluated PET state is
    /// retained for the new cursor; the winner is never replayed merely to
    /// commit it. A solved winner is synchronously passed to writeback.
    pub fn check(&self, attempt: AttemptId, attempts: &[String]) -> Result<CheckResult> {
        let attempts = validated_attempts(attempts)?;
        let project = self.attempt_project(attempt)?;
        let traces = self.traces_for_project(&project)?;
        let (project, project_gate) = self.project_access(&project)?;
        let gate = project_gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let view = traces.inspect(attempt.0).map_err(trace_error)?;
        let base = self.pet_state_for_attempt(&project, attempt, view.root(), view.actions())?;
        let mut rejected = Vec::new();
        let mut winner = None;
        for (index, tactics) in attempts.iter().enumerate() {
            match self.evaluate_attempt(&project, view.root(), view.actions(), &base, tactics) {
                Ok(native) => {
                    winner = Some((index, tactics, native));
                    break;
                }
                Err(error) => rejected.push(error),
            }
        }

        let Some((selected, tactics, native)) = winner else {
            return Ok(CheckResult {
                selected: None,
                state: self.state_from_pet(attempt, view.root(), &base, ProofLifecycle::Open),
                rejected,
                error: None,
            });
        };

        // Design note: PET acceptance precedes topology publication, so no
        // rejected fragment (including an accepted prefix followed by a
        // rejected sentence) becomes a TraceForest branch.
        let mut cursor = attempt.0;
        for tactic in tactics {
            let key = ActionKey::new(tactic.0.as_bytes())
                .expect("validated_attempts already bounded every action key");
            cursor = traces
                .step(cursor, key, || Ok::<_, Error>(tactic.clone()))
                .map_err(trace_call_error)?;
        }
        let mut committed_actions = view.actions().to_vec();
        committed_actions.extend(tactics.iter().cloned());
        self.pet.retain_state(&project, &native, &committed_actions);
        self.remember_attempt(cursor, &project);
        self.remember_pet_state(
            AttemptId(cursor),
            &native,
            view.actions().len() + tactics.len(),
        );
        let mut state = self.state_from_pet(
            AttemptId(cursor),
            view.root(),
            &native,
            ProofLifecycle::Open,
        );
        drop(gate);
        let mut close_error = None;
        match self.close_solved(AttemptId(cursor), &native) {
            Ok(Some((published, error))) => {
                state = published;
                close_error = error;
            }
            Ok(None) => {}
            Err(error) => {
                // Design note: an outer NotFound means the trace was retired
                // before close acquired it (for example, a concurrent close
                // won). Never expose an AttemptId/checkpoint that is already
                // unusable. Callback failures take the Ok(Some(...)) path and
                // retain their live attempt instead.
                if matches!(
                    error.kind,
                    ErrorKind::NotFound | ErrorKind::DeclarationChanged
                ) {
                    state.attempt = None;
                }
                close_error = Some(error);
            }
        }

        Ok(CheckResult {
            selected: Some(selected),
            state,
            rejected,
            error: close_error,
        })
    }

    /// Evaluate every proof fragment from the same PET base without appending
    /// trace edges, selecting a cursor, publishing source, or closing a proof.
    pub fn try_attempts(
        &self,
        attempt: AttemptId,
        attempts: &[String],
    ) -> Result<Vec<AttemptResult>> {
        let attempts = validated_attempts(attempts)?;
        let project = self.attempt_project(attempt)?;
        let traces = self.traces_for_project(&project)?;
        let (project, project_gate) = self.project_access(&project)?;
        let _gate = project_gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let view = traces.inspect(attempt.0).map_err(trace_error)?;
        let base = self.pet_state_for_attempt(&project, attempt, view.root(), view.actions())?;
        let mut out = Vec::with_capacity(attempts.len());
        for tactics in &attempts {
            match self.evaluate_attempt(&project, view.root(), view.actions(), &base, tactics) {
                Ok(native) => {
                    let mut state =
                        self.state_from_pet(attempt, view.root(), &native, ProofLifecycle::Open);
                    // Design note: hypothetical goals do not correspond to a
                    // committed TraceForest cursor. Do not attach the shared
                    // base AttemptId to a state that cannot be selected.
                    state.attempt = None;
                    out.push(AttemptResult {
                        solved: native.proof_finished && native.goals.all_clear(),
                        state: Some(state),
                        error: None,
                    });
                }
                Err(error) => out.push(AttemptResult {
                    solved: false,
                    state: None,
                    error: Some(error),
                }),
            }
        }
        Ok(out)
    }

    /// Run one already validated fragment on a persistent PET branch.
    ///
    /// `base_actions` is the replay prefix for `base`. Each later recovery
    /// receives that prefix plus only the sentences accepted earlier in this
    /// fragment. No engine or TraceForest state is changed.
    fn evaluate_attempt(
        &self,
        project: &Path,
        root: &DeclarationSource,
        base_actions: &[CanonicalTactic],
        base: &pet::PetState,
        tactics: &[CanonicalTactic],
    ) -> Result<pet::PetState> {
        let mut state = base.clone();
        let mut replay = base_actions.to_vec();
        for tactic in tactics {
            let next = self
                .pet
                .fork_state(project, root, &state, &replay, tactic)
                .map_err(|error| self.pet_error(error))?;
            replay.push(tactic.clone());
            state = next;
        }
        Ok(state)
    }

    /// Returns PET's structured goals, replaying only if its live handle is gone.
    pub fn inspect(&self, attempt: AttemptId) -> Result<ProofState> {
        self.inspect_inner(attempt)
    }

    /// Restore an existing trace attempt without pruning the immutable forest.
    ///
    /// `current` and `target` must be live attempts rooted at the same source
    /// declaration and snapshot. PET validates or replays `target` before its
    /// state is returned. Request-boundary topology deliberately remains an
    /// MCP concern; this engine operation accepts only opaque trace handles.
    pub fn checkout(&self, current: AttemptId, target: AttemptId) -> Result<ProofState> {
        let project = self.attempt_project(current)?;
        let target_project = self.attempt_project(target)?;
        if target_project != project {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "target proof attempt belongs to another project",
            ));
        }
        let traces = self.traces_for_project(&project)?;
        let (project, project_gate) = self.project_access(&project)?;
        let gate = project_gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let current_view = traces.inspect(current.0).map_err(trace_error)?;
        let target_view = traces.inspect(target.0).map_err(trace_error)?;
        if current_view.root() != target_view.root() {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "target proof attempt belongs to another proof",
            ));
        }
        // Design note: PET's cached handle is only a fast path. This call
        // transparently replays the immutable target trace after eviction.
        // Selection changes only in the MCP layer after checkout succeeds.
        let native = self.pet_state_for_attempt(
            &project,
            target,
            target_view.root(),
            target_view.actions(),
        )?;
        let state = self.state_from_pet(target, target_view.root(), &native, ProofLifecycle::Open);
        drop(gate);
        Ok(state)
    }

    fn inspect_inner(&self, attempt: AttemptId) -> Result<ProofState> {
        let project = self.attempt_project(attempt)?;
        let traces = self.traces_for_project(&project)?;
        let (project, project_gate) = self.project_access(&project)?;
        let _gate = project_gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let view = traces.inspect(attempt.0).map_err(trace_error)?;
        let native = self.pet_state_for_attempt(&project, attempt, view.root(), view.actions())?;
        Ok(self.state_from_pet(attempt, view.root(), &native, ProofLifecycle::Open))
    }
}

// Read-only PET query operations. MCP owns request dispatch; the engine exposes
// only concrete operations rather than retaining a second request DTO layer.
impl Engine {
    /// Returns PET's current goal state for an attempt owned by `project`.
    pub fn query_goals(&self, project: &Path, attempt: AttemptId) -> Result<ProofState> {
        let actual = self.attempt_project(attempt)?;
        let requested = self
            .project_state(project)?
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .root
            .clone();
        if requested != actual {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "query project does not match attempt",
            ));
        }
        self.inspect(attempt)
    }

    /// Runs Rocq `Search` in an attempt or at an explicitly named declaration.
    pub fn query_search(
        &self,
        project: &Path,
        context: Option<AttemptId>,
        pattern: String,
        at: Option<&DeclarationIdentity>,
    ) -> Result<String> {
        validate_native_fragment(&pattern)?;
        self.query_in_context(project, context, at, pet::FixedPetQuery::Search(pattern))
    }

    /// Returns PET/Rocq `About` output for an exact declaration identity.
    pub fn query_statement(&self, project: &Path, id: &DeclarationIdentity) -> Result<String> {
        self.query_declaration(project, id, pet::FixedPetQuery::About)
    }

    /// Returns PET/Rocq `Print` output for an exact declaration identity.
    pub fn query_proof(&self, project: &Path, id: &DeclarationIdentity) -> Result<String> {
        self.query_declaration(project, id, pet::FixedPetQuery::Print)
    }

    /// Returns PET/Rocq `Print` output for an exact definition identity.
    pub fn query_definition(&self, project: &Path, id: &DeclarationIdentity) -> Result<String> {
        self.query_declaration(project, id, pet::FixedPetQuery::Print)
    }

    /// Returns PET/Rocq `Print Assumptions` output for a declaration.
    pub fn query_assumptions(&self, project: &Path, id: &DeclarationIdentity) -> Result<String> {
        self.query_declaration(project, id, pet::FixedPetQuery::Assumptions)
    }

    /// Returns PET/Rocq dependency output for a declaration.
    pub fn query_dependencies(&self, project: &Path, id: &DeclarationIdentity) -> Result<String> {
        self.query_declaration(project, id, pet::FixedPetQuery::Dependencies)
    }

    /// Asks PET to type an expression in an attempt or named source context.
    pub fn query_expression_type(
        &self,
        project: &Path,
        context: Option<AttemptId>,
        expression: String,
        at: Option<&DeclarationIdentity>,
    ) -> Result<String> {
        validate_native_fragment(&expression)?;
        self.query_in_context(
            project,
            context,
            at,
            pet::FixedPetQuery::ExpressionType(expression),
        )
    }

    /// Asks PET to interpret notation in an attempt or named source context.
    pub fn query_notation(
        &self,
        project: &Path,
        context: Option<AttemptId>,
        expression: String,
        at: Option<&DeclarationIdentity>,
    ) -> Result<String> {
        validate_native_fragment(&expression)?;
        self.query_in_context(
            project,
            context,
            at,
            pet::FixedPetQuery::Notation(expression),
        )
    }

    fn query_declaration(
        &self,
        project: &Path,
        identity: &DeclarationIdentity,
        make_query: impl FnOnce(String) -> pet::FixedPetQuery,
    ) -> Result<String> {
        let project = self.load_declaration(project, identity)?;
        let (_, project_gate) = self.project_access(&project)?;
        let _gate = project_gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = self.source_for_identity(&project, identity)?;
        self.query_text(
            &project,
            None,
            Some(&source),
            make_query(identity.constant().unwrap_or_default().to_owned()),
        )
    }

    fn query_in_context(
        &self,
        project: &Path,
        context: Option<AttemptId>,
        at: Option<&DeclarationIdentity>,
        query: pet::FixedPetQuery,
    ) -> Result<String> {
        if context.is_some() && at.is_some() {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "query cannot combine the selected proof with an explicit declaration context",
            ));
        }
        let (project, project_gate) = self.project_access(project)?;
        let _gate = project_gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = self.query_context_source(&project, at)?;
        self.query_text(&project, context, source.as_ref(), query)
    }

    /// Run a fixed PET query in either the selected interactive state or the
    /// original checked source document. No synthetic theorem is constructed.
    fn query_text(
        &self,
        project: &Path,
        context: Option<AttemptId>,
        target_source: Option<&DeclarationSource>,
        query: pet::FixedPetQuery,
    ) -> Result<String> {
        if let Some(attempt) = context {
            let actual = self.attempt_project(attempt)?;
            let requested = self
                .project_state(project)?
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .root
                .clone();
            if requested != actual {
                return Err(Error::new(
                    ErrorKind::DeclarationChanged,
                    "query project does not match attempt",
                ));
            }
            let view = self
                .traces_for_attempt(attempt)?
                .inspect(attempt.0)
                .map_err(trace_error)?;
            let state =
                self.pet_state_for_attempt(project, attempt, view.root(), view.actions())?;
            let (text, state) = self
                .pet
                .run_fixed(project, view.root(), &state, view.actions(), query)
                .map_err(|error| self.pet_error(error))?;
            self.remember_pet_state(attempt, &state, view.actions().len());
            return Ok(text);
        }

        let source = target_source.ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidRequest,
                "query requires a selected proof or an explicit 'at' declaration",
            )
        })?;
        self.pet
            .query_after_declaration(project, source, query)
            .map_err(|error| self.pet_error(error))
    }

    fn query_context_source(
        &self,
        project: &Path,
        at: Option<&DeclarationIdentity>,
    ) -> Result<Option<DeclarationSource>> {
        at.map(|identity| {
            let project = self.load_declaration(project, identity)?;
            self.source_for_identity(&project, identity)
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration_source(file: &str) -> DeclarationSource {
        DeclarationSource {
            info: DeclarationInfo {
                identity: DeclarationIdentity {
                    file: FileId(file.to_owned()),
                    qualified_path: vec!["Demo".into(), "Unit".into(), "same".into()],
                },
                kind: DeclarationKind::Theorem,
                statement: "True".into(),
            },
            library: LogicalLibrary(vec!["Demo".into(), "Unit".into()]),
            anchor: PetAnchor {
                source: PathBuf::from(file),
                digest: [7; 32],
                header: PetRange { start: 0, end: 10 },
                declaration: Some(PetRange { start: 0, end: 20 }),
            },
        }
    }

    #[test]
    fn trace_root_key_contains_the_complete_declaration_identity() {
        let left = declaration_source("left/Unit.v");
        let right = declaration_source("right/Unit.v");
        assert_ne!(root_key(&left).unwrap(), root_key(&right).unwrap());
    }
}
