//! Stateless sequencing facade over the Dune, PET, and writeback wrappers.
//!
//! Active projects and checkpoint topology belong to `rocq-mcp`; `Engine`
//! retains only operator configuration and never stores declarations, attempts,
//! traces, or PET state IDs.

use crate::types::{
    ArtifactFingerprint, GoalDetail, GoalHypothesis, GoalStackDetail, PetRange, PetWorkspace,
    SourceAnchor, StructuredGoals,
};
use crate::{dune, pet, writeback, *};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
};

/// Canonical Dune workspace identity plus its current Dune-selected view.
///
/// Clones share the view so one project runtime can refresh it atomically at
/// request admission without replacing the runtime or its PET actor.
#[derive(Clone)]
pub struct DuneProject {
    root: PathBuf,
    layout: Arc<RwLock<dune::Layout>>,
    /// Cheap filesystem-only hint used to avoid spawning `dune describe` for
    /// every read-only MCP query.  It tracks layout inputs (Dune files and
    /// selected source names), not source contents; artifact changes are
    /// coordinated separately by `ArtifactFingerprint`.
    probe: Arc<RwLock<[u8; 32]>>,
}

impl DuneProject {
    pub fn id(&self) -> &Path {
        &self.root
    }

    fn source(&self, file: &FileId) -> Result<PathBuf> {
        validate_file_id(file)?;
        let path = fs::canonicalize(self.root.join(&file.0))
            .map_err(|_| Error::new(ErrorKind::NotFound, "requested source file is unavailable"))?;
        if !self.read_layout().contains_file(&path) {
            return Err(Error::new(
                ErrorKind::NotFound,
                "Dune has not selected the requested source file",
            ));
        }
        Ok(path)
    }

    pub(crate) fn pet_workspace(&self, source: &Path) -> Result<PetWorkspace> {
        self.read_layout().pet_workspace(source)
    }

    pub(crate) fn build_target(&self, source: &Path) -> Result<PathBuf> {
        self.read_layout().build_target(source)
    }

    /// Resolve the compiler-produced `.glob` beside one selected `.vo`.
    /// Dune's reported target is relative to the active `default` context;
    /// honor an absolute or relative `DUNE_BUILD_DIR` without inventing a
    /// second project layout.
    fn glob_path(&self, source: &Path) -> Result<PathBuf> {
        let target = self.build_target(source)?;
        let build_root = self.build_root();
        let mut path = build_root.join("default").join(target);
        path.set_extension("glob");
        Ok(path)
    }

    fn build_root(&self) -> PathBuf {
        match std::env::var_os("DUNE_BUILD_DIR") {
            Some(value) => {
                let value = PathBuf::from(value);
                if value.is_absolute() {
                    value
                } else {
                    self.root.join(value)
                }
            }
            None => self.root.join("_build"),
        }
    }

    fn artifact_path(&self, source: &Path) -> Result<PathBuf> {
        Ok(self
            .build_root()
            .join("default")
            .join(self.build_target(source)?))
    }

    /// Return Dune's build-context copy of a source file.  PET must open this
    /// path so Rocq derives the same logical module name as the compiler's
    /// `-R . Prefix` action; opening the source-tree path would make the
    /// workspace root contribute `_build.default` to dependency names.
    fn pet_source(&self, source: &Path) -> Result<PathBuf> {
        let target = self.build_target(source)?;
        let mut path = self.build_root().join("default").join(target);
        path.set_extension("v");
        if path.is_file() {
            Ok(path)
        } else {
            // A caller that only asks for layout metadata may reach this
            // method before the first build.  The coordinator normally builds
            // before PET use; retaining the source fallback keeps declaration
            // validation deterministic for that narrow diagnostic path.
            Ok(source.to_owned())
        }
    }

    fn library(&self, source: &Path) -> Result<LogicalLibrary> {
        self.read_layout().library(source).cloned()
    }

    fn source_for_constant(&self, constant: &str) -> Option<PathBuf> {
        self.read_layout().source_for_constant(constant)
    }

    fn files(&self) -> Vec<PathBuf> {
        self.read_layout().files()
    }

    pub fn file_count(&self) -> usize {
        self.read_layout().files().len()
    }

    fn replace_layout(&self, layout: dune::Layout) -> bool {
        let mut current = self
            .layout
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *current == layout {
            return false;
        }
        *current = layout;
        true
    }

    fn read_layout(&self) -> std::sync::RwLockReadGuard<'_, dune::Layout> {
        self.layout
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn probe(&self) -> [u8; 32] {
        *self
            .probe
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn replace_probe(&self, probe: [u8; 32]) {
        *self
            .probe
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = probe;
    }
}

/// Compute a cheap, deterministic hint for inputs that can change Dune's
/// selected source/layout view.  Source contents are intentionally excluded;
/// their compiled consumer identity is handled by `ArtifactFingerprint`.
fn layout_probe(root: &Path) -> Result<[u8; 32]> {
    let mut entries = Vec::<(String, Option<Vec<u8>>)>::new();
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut Vec<(String, Option<Vec<u8>>)>,
    ) -> std::io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            if name == ".git" || name == "_build" || name == "target" {
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                visit(root, &path, entries)?;
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let is_source = path.extension().is_some_and(|extension| extension == "v");
            let is_layout = matches!(
                name.to_str(),
                Some("dune")
                    | Some("dune-project")
                    | Some("dune-workspace")
                    | Some("dune-workspace.dev")
                    | Some("_CoqProject")
                    | Some("_RocqProject")
            );
            if is_source {
                entries.push((format!("source:{relative}"), None));
            } else if is_layout {
                entries.push((format!("layout:{relative}"), Some(fs::read(&path)?)));
            }
        }
        Ok(())
    }
    visit(root, root, &mut entries).map_err(|_| {
        Error::new(
            ErrorKind::InvalidConfiguration,
            "project layout inputs are unavailable",
        )
    })?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (name, contents) in entries {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        if let Some(contents) = contents {
            hasher.update(contents);
        }
        hasher.update([0xff]);
    }
    Ok(hasher.finalize().into())
}

fn digest_if_present(path: &Path) -> Result<Option<[u8; 32]>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(Sha256::digest(bytes).into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "Dune artifact is unavailable",
        )),
    }
}

/// Thin operation facade. It is safe to share because it has no live project
/// or proof state.
pub struct Engine {
    config: EngineConfig,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        Ok(Self { config })
    }

    /// Resolve a caller path through Dune and retain its typed selected layout.
    /// PET is not started by attachment.
    pub fn attach(&self, requested: &Path) -> Result<DuneProject> {
        let requested = fs::canonicalize(requested).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "project path is unavailable",
            )
        })?;
        let (root, layout) = dune::Layout::load(&requested, self.config.command_timeout)?;
        let probe = layout_probe(&root)?;
        Ok(DuneProject {
            root,
            layout: Arc::new(RwLock::new(layout)),
            probe: Arc::new(RwLock::new(probe)),
        })
    }

    /// Probe layout inputs and, only when they changed, re-query Dune and
    /// atomically replace the cached typed view when its selected files,
    /// libraries, targets, or PET load paths changed.
    ///
    /// Returns `true` exactly when the caller must begin a new PET epoch and
    /// invalidate all exported state IDs for this project. A workspace-root
    /// identity change fails closed and requires an explicit `start`.
    pub fn refresh_project(&self, project: &DuneProject) -> Result<bool> {
        let probe = layout_probe(&project.root)?;
        if probe == project.probe() {
            return Ok(false);
        }
        let (root, layout) = dune::Layout::load(&project.root, self.config.command_timeout)?;
        if root != project.root {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune workspace identity changed; call start again",
            ));
        }
        let changed = project.replace_layout(layout);
        project.replace_probe(probe);
        Ok(changed)
    }

    /// Return only Dune-selected source identities.
    pub fn list_files(&self, project: &DuneProject) -> Result<Vec<FileId>> {
        project
            .files()
            .iter()
            .map(|path| {
                FileId::from_path(&project.root, path)
                    .map_err(|message| Error::new(ErrorKind::InvalidConfiguration, message))
            })
            .collect()
    }

    /// Ask PET once for one document and project supported proof declarations
    /// onto the public identity/type shape.
    pub fn list_decls(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        file: &FileId,
    ) -> Result<Vec<DeclarationInfo>> {
        Ok(self
            .declarations(project, actor, file)?
            .into_iter()
            .filter_map(|declaration| declaration.target.map(|target| target.info))
            .collect())
    }

    /// Locate declarations across Dune-selected source files. PET remains the
    /// authority for declaration identity and statements; a cheap source
    /// candidate pass prevents a full semantic walk of large workspaces.
    pub fn locate_symbols(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        symbol: &str,
        limit: usize,
    ) -> Result<Vec<SymbolLocation>> {
        self.locate_symbols_with_progress(project, actor, symbol, limit, |_, _, _| {})
    }

    /// Locate declarations without asking PET to check every source document.
    /// The first pass is a bounded, byte-level candidate index owned by the
    /// engine: a declaration whose leaf is `needle` must contain that exact
    /// identifier in its source file.  PET remains authoritative for the
    /// second pass, which checks only candidate documents and supplies the
    /// canonical declaration metadata and statement.
    ///
    /// `progress` receives `(completed_files, total_files, phase_summary)` and
    /// is called between files, so request cancellation can terminate a large
    /// workspace scan without leaving a PET request in flight.
    pub fn locate_symbols_with_progress(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        symbol: &str,
        limit: usize,
        mut progress: impl FnMut(u64, u64, &str),
    ) -> Result<Vec<SymbolLocation>> {
        let files =
            self.locate_symbol_candidates(project, symbol, |completed, total, summary| {
                progress(completed, total, summary);
            })?;
        self.resolve_symbol_candidates(
            project,
            actor,
            symbol,
            limit,
            &files,
            |completed, total, summary| progress(completed, total, summary),
        )
    }

    /// Build a cheap, conservative file candidate set for a symbol.  A fresh
    /// `.glob` can exclude non-matching files; a missing/stale index falls back
    /// to identifier-bounded source text so it can never create a false
    /// negative merely because Dune has not rebuilt yet.
    pub fn locate_symbol_candidates(
        &self,
        project: &DuneProject,
        symbol: &str,
        mut progress: impl FnMut(u64, u64, &str),
    ) -> Result<Vec<FileId>> {
        let symbol = symbol.trim();
        if symbol.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "symbol must not be empty",
            ));
        }
        let files = self.list_files(project)?;
        let total = files.len() as u64;
        let needle = symbol.rsplit('.').next().unwrap_or(symbol);
        let mut candidate_files = Vec::new();
        // An exact qualified name has one Dune-owned compilation unit. Avoid
        // even the cheap candidate scan in that common, unambiguous case.
        if !symbol.contains('.') || project.source_for_constant(symbol).is_none() {
            for (index, file) in files.into_iter().enumerate() {
                if crate::request_cancelled() {
                    return Err(Error::new(
                        ErrorKind::RequestCancelled,
                        "symbol lookup cancelled",
                    ));
                }
                let source = project.source(&file)?;
                let indexed = project
                    .glob_path(&source)
                    .ok()
                    .and_then(|glob| glob_matches(&source, &glob, symbol));
                let candidate = match indexed {
                    Some(matches) => matches,
                    None => fs::read(&source)
                        .map(|bytes| contains_identifier(&String::from_utf8_lossy(&bytes), needle))
                        .map_err(|_| {
                            Error::new(
                                ErrorKind::DeclarationChanged,
                                "declaration source is unavailable",
                            )
                        })?,
                };
                if candidate {
                    candidate_files.push(file);
                }
                progress(index as u64 + 1, total, "indexed source candidates");
            }
        } else if let Some(source) = project.source_for_constant(symbol) {
            let file = FileId::from_path(&project.root, &source)
                .map_err(|message| Error::new(ErrorKind::InvalidConfiguration, message))?;
            candidate_files.push(file);
            progress(total, total, "selected exact Dune compilation unit");
        }
        Ok(candidate_files)
    }

    /// Ask PET for canonical declarations only in the supplied candidate
    /// documents.  The caller must first ensure Dune's build-context source
    /// copies and consumer artifacts are current, then coordinate any PET
    /// epoch transition before invoking this method.
    pub fn resolve_symbol_candidates(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        symbol: &str,
        limit: usize,
        candidate_files: &[FileId],
        mut progress: impl FnMut(u64, u64, &str),
    ) -> Result<Vec<SymbolLocation>> {
        let symbol = symbol.trim();
        if symbol.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidRequest,
                "symbol must not be empty",
            ));
        }
        // Keep one slot available for the MCP adapter's truncation probe. The
        // public schema caps callers at 100, but an internal 101st candidate
        // is needed to report `truncated:true` when exactly 100 match.
        let limit = limit.clamp(1, 101);
        let mut candidates = Vec::new();
        let candidate_total = candidate_files.len() as u64;
        for (candidate_index, file) in candidate_files.iter().enumerate() {
            if crate::request_cancelled() {
                return Err(Error::new(
                    ErrorKind::RequestCancelled,
                    "symbol lookup cancelled",
                ));
            }
            let source = project.source(file)?;
            let library = project.library(&source)?;
            for declaration in self.list_decls(project, actor, file)? {
                let qualified_name = declaration.identity.qualified_name();
                let leaf = declaration
                    .identity
                    .qualified_path
                    .last()
                    .map(String::as_str)
                    .unwrap_or_default();
                let match_kind = symbol_match_kind(&qualified_name, leaf, symbol);
                let Some(match_kind) = match_kind else {
                    continue;
                };
                let module = library.0.join(".");
                candidates.push(SymbolLocation {
                    identity: declaration.identity,
                    qualified_name,
                    kind: declaration.kind,
                    statement: declaration.statement,
                    require_import: format!("Require Import {module}."),
                    match_kind: match_kind.to_owned(),
                });
                if candidates.len() >= limit {
                    // The adapter asks for one extra row to distinguish an
                    // exact limit from a truncated result.  Stop semantic
                    // checking once that bound is reached; no unbounded PET
                    // document walk is allowed for a broad leaf query.
                    break;
                }
            }
            progress(
                candidate_index as u64 + 1,
                candidate_total,
                "resolved candidate declarations with PET",
            );
            if candidates.len() >= limit {
                break;
            }
        }
        candidates.sort_by(|left, right| {
            match_rank(&left.match_kind)
                .cmp(&match_rank(&right.match_kind))
                .then_with(|| left.qualified_name.cmp(&right.qualified_name))
                .then_with(|| left.identity.file.cmp(&right.identity.file))
        });
        candidates.truncate(limit);
        Ok(candidates)
    }

    /// Select an exact PET declaration. PET-finished declarations are returned
    /// as `Published` so MCP can perform the required project epoch transition.
    pub fn open_declaration(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        identity: DeclarationIdentity,
    ) -> Result<OpenResult> {
        validate_identity(&identity)?;
        let resolved = self.resolve(project, actor, &identity)?;
        let target = resolved.target.ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidDeclaration,
                "target is not a proof declaration",
            )
        })?;
        if resolved.proof_finished {
            return Ok(OpenResult::Published(target));
        }
        if !target.anchor.replaceable {
            return Err(Error::new(
                ErrorKind::InvalidDeclaration,
                "declaration shares one source command with another declaration",
            ));
        }
        self.open_target(project, actor, target)
            .map(|opened| OpenResult::Open(Box::new(opened)))
    }

    /// Validate a new declaration header at PET's exact module insertion state.
    ///
    /// `file` is a Dune-selected source identity. `name` is local or
    /// nested-module-qualified relative to the compilation unit; Dune alone
    /// supplies the logical prefix. Disk is untouched until publication.
    pub fn declare(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        kind: DeclarationKind,
        file: FileId,
        name: &str,
        statement: &str,
    ) -> Result<OpenedProof> {
        let source = project.source(&file)?;
        let library = project.library(&source)?;
        let identity = declared_identity(file, &library, name)?;
        validate_identity(&identity)?;
        let statement = statement.trim().trim_end_matches('.').trim();
        if statement.is_empty() {
            return Err(Error::new(
                ErrorKind::InvalidDeclaration,
                "declaration statement is empty",
            ));
        }
        let relative = relative_path(&identity, &library)?;
        let (leaf, modules) = relative.split_last().ok_or_else(|| {
            Error::new(ErrorKind::InvalidDeclaration, "declaration path is empty")
        })?;
        if self
            .declarations(project, actor, &identity.file)?
            .into_iter()
            .any(|declaration| declaration.identity == identity)
        {
            return Err(Error::new(
                ErrorKind::InvalidDeclaration,
                "declaration identity is already present",
            ));
        }
        let workspace = project.pet_workspace(&source)?;
        let pet_source = project.pet_source(&source)?;
        let insertion = actor
            .insertion_point(&workspace, &pet_source, modules)
            .map_err(declaration_pet_error)?;
        let bytes = fs::read(&source).map_err(|_| {
            Error::new(
                ErrorKind::DeclarationChanged,
                "target source is unavailable",
            )
        })?;
        if insertion > bytes.len() {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "declaration insertion metadata is invalid",
            ));
        }
        let header = format!("{} {} : {}", kind.keyword(), leaf, statement);
        let base = actor
            .state_at(&workspace, &pet_source, insertion)
            .map_err(declaration_pet_error)?;
        let opened = actor
            .run(base.state, &format!("{header}."))
            .map_err(declaration_pet_error);
        let opened = match release_after(actor, base.state, opened) {
            Ok(opened) => opened,
            Err(error) => {
                return Err(enrich_declaration_error(
                    self, project, actor, error, statement, kind, leaf,
                ));
            }
        };
        if let Err(error) = require_open_proof(&opened) {
            actor
                .release_states(&[opened.state])
                .map_err(declaration_pet_error)?;
            return Err(error);
        }
        let info = DeclarationInfo {
            identity,
            kind,
            statement: header.clone(),
        };
        let target = DeclarationTarget {
            info: info.clone(),
            anchor: SourceAnchor {
                source,
                digest: Sha256::digest(&bytes).into(),
                header: PetRange {
                    start: insertion,
                    end: insertion,
                },
                declaration: PetRange {
                    start: insertion,
                    end: insertion,
                },
                replaceable: true,
            },
            new_header: Some(header),
        };
        Ok(OpenedProof {
            target,
            state: opened.state,
            view: proof_state(info, &opened.goals),
            finished: opened.proof_finished,
        })
    }

    /// Recreate only a proof root. MCP replays its own root-to-checkpoint list.
    pub fn replay_root(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
    ) -> Result<OpenedProof> {
        self.validate_target(project, target)?;
        if target.is_new() {
            let workspace = project.pet_workspace(&target.anchor.source)?;
            let base = actor
                .state_at(
                    &workspace,
                    &project.pet_source(&target.anchor.source)?,
                    target.anchor.header.start,
                )
                .map_err(declaration_pet_error)?;
            let header = target.new_header.as_deref().expect("new target has header");
            let opened = actor
                .run(base.state, &format!("{header}."))
                .map_err(declaration_pet_error);
            let opened = release_after(actor, base.state, opened)?;
            if let Err(error) = require_open_proof(&opened) {
                actor
                    .release_states(&[opened.state])
                    .map_err(declaration_pet_error)?;
                return Err(error);
            }
            return Ok(OpenedProof {
                target: target.clone(),
                state: opened.state,
                view: proof_state(target.info.clone(), &opened.goals),
                finished: opened.proof_finished,
            });
        }
        let current = self
            .resolve(project, actor, &target.info.identity)
            .map_err(|error| {
                if error.kind == ErrorKind::NotFound {
                    Error::new(
                        ErrorKind::DeclarationChanged,
                        "target identity changed in the current project context",
                    )
                } else {
                    error
                }
            })?;
        let current = current
            .target
            .ok_or_else(|| Error::new(ErrorKind::DeclarationChanged, "declaration kind changed"))?;
        if current.info != target.info || current.anchor.digest != target.anchor.digest {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "target declaration changed while proof was open",
            ));
        }
        self.open_target(project, actor, current)
    }

    /// Execute one complete proof fragment directly through PET.
    pub fn run(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        state: pet::PetStateId,
        fragment: &str,
        timeout: Option<std::time::Duration>,
    ) -> Result<ProofStep> {
        self.validate_target(project, target)?;
        validate_fragment(fragment)?;
        let execution = actor
            .run_with_timeout(state, fragment, timeout)
            .map_err(step_pet_error)?;
        if !execution.goals.proof_mode || !execution.goals.given_up.is_empty() {
            actor
                .release_states(&[execution.state])
                .map_err(step_pet_error)?;
            return Err(Error::new(
                ErrorKind::ProofStepFailed,
                "fragment closes proof mode, runs a global command, or gives up a goal",
            ));
        }
        let mut view = proof_state(target.info.clone(), &execution.goals);
        view.structured_goals = Some(structured_goals(&execution.goals));
        Ok(ProofStep {
            state: execution.state,
            view,
            finished: execution.proof_finished,
        })
    }

    /// Execute a proof fragment with Rocq-parser sentence diagnostics.  Unlike
    /// [`Self::run`], a semantic rejection is returned as a value containing
    /// the state immediately before the failed command.  That temporary PET
    /// state is always released before this method returns, so callers cannot
    /// accidentally mutate or retain a partial prefix.
    #[allow(clippy::too_many_arguments)]
    pub fn run_traced(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        state: pet::PetStateId,
        fragment: &str,
        include_trace: bool,
        timeout: Option<std::time::Duration>,
    ) -> Result<TracedProofStep> {
        self.validate_target(project, target)?;
        validate_fragment(fragment)?;
        let outcome = actor
            .run_traced_with_timeout(state, fragment, include_trace, timeout)
            .map_err(step_pet_error)?;
        match outcome {
            pet::PetTraceOutcome::Success(execution) => {
                checked_trace_success(actor, target, execution)
            }
            pet::PetTraceOutcome::Failure(failure) => {
                let mut before = proof_state(target.info.clone(), &failure.before_goals);
                before.structured_goals = Some(structured_goals(&failure.before_goals));
                let result = TracedProofStep::Failure(ProofFailure {
                    error: step_pet_error(failure.error),
                    sentence_index: failure.sentence_index,
                    sentence_range: failure.sentence_range,
                    before,
                    trace: failure
                        .trace
                        .into_iter()
                        .map(|step| ProofTraceStep {
                            sentence_index: step.sentence_index,
                            byte_range: ProofDiagnostic {
                                byte_start: step.byte_start,
                                byte_end: step.byte_end,
                            },
                            command: step.command,
                        })
                        .collect(),
                });
                actor
                    .release_states(&[failure.before_state])
                    .map_err(step_pet_error)?;
                Ok(result)
            }
        }
    }

    pub fn goals(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        state: pet::PetStateId,
        scope: GoalScope,
        goal_id: Option<&[serde_json::Value]>,
    ) -> Result<ProofState> {
        self.validate_target(project, target)?;
        let goals = actor.goals(state).map_err(step_pet_error)?;
        if !goals.proof_mode {
            return Err(Error::new(
                ErrorKind::NotFound,
                "proof state is no longer open",
            ));
        }
        let rendered = render_goals(&goals, scope, goal_id)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "goal_id is not in this state"))?;
        let mut result = proof_state_with_rendering(target.info.clone(), &goals, rendered);
        result.structured_goals = Some(structured_goals(&goals));
        Ok(result)
    }

    /// Run a semantic query in a caller-owned proof state.
    pub fn query_state(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        state: pet::PetStateId,
        query: PetQuery,
    ) -> Result<String> {
        self.validate_target(project, target)?;
        actor.query(state, &query).map_err(query_pet_error)
    }

    /// Run a semantic query after one exact source declaration, releasing the
    /// temporary context state before returning.
    ///
    /// `release_states` drops only the exported immutable snapshot handle. It
    /// does not unload the PET process, workspace, or PET's checked-document
    /// cache, so querying another file in the same project still uses the
    /// same actor and loaded Dune environment. Keeping this one-shot context
    /// alive would add wrapper-owned state without changing query semantics.
    pub fn query_at(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        identity: &DeclarationIdentity,
        query: PetQuery,
    ) -> Result<String> {
        let resolved = self.resolve(project, actor, identity)?;
        let target = resolved.target.ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidDeclaration,
                "query target is not supported",
            )
        })?;
        let workspace = project.pet_workspace(&target.anchor.source)?;
        let pet_source = project.pet_source(&target.anchor.source)?;
        let context = actor
            .state_at(&workspace, &pet_source, target.anchor.declaration.end)
            .map_err(query_pet_error)?;
        let result = actor.query(context.state, &query).map_err(query_pet_error);
        let released = actor
            .release_states(&[context.state])
            .map_err(query_pet_error);
        match released {
            Ok(()) => result,
            Err(error) => Err(error),
        }
    }

    /// Return the current source/consumer-artifact identity without spawning
    /// Dune.  The MCP coordinator uses this cheap probe to skip a no-op build
    /// while still noticing an external source or `.vo`/`.glob` replacement.
    pub fn artifact_fingerprint(
        &self,
        project: &DuneProject,
        file: &FileId,
    ) -> Result<ArtifactFingerprint> {
        let source = project.source(file)?;
        let source_bytes = fs::read(&source).map_err(|_| {
            Error::new(
                ErrorKind::DeclarationChanged,
                "target source is unavailable",
            )
        })?;
        let artifact = project.artifact_path(&source)?;
        let vo = digest_if_present(&artifact)?;
        let mut glob = artifact.clone();
        glob.set_extension("glob");
        let glob = digest_if_present(&glob)?;
        Ok(ArtifactFingerprint {
            source: Sha256::digest(source_bytes).into(),
            vo,
            glob,
        })
    }

    /// Build exactly one Dune consumer target and return its resulting
    /// fingerprint.  PET is deliberately not touched here: the MCP project
    /// coordinator must invalidate all session state handles before changing
    /// the PET epoch.
    pub fn reconcile_source(
        &self,
        project: &DuneProject,
        file: &FileId,
    ) -> Result<ArtifactFingerprint> {
        let source = project.source(file)?;
        writeback::native_build(project, &source, self.config.command_timeout)?;
        self.artifact_fingerprint(project, file)
    }

    /// Build, refresh, reopen, and trust-audit a PET-finished target.
    /// The MCP caller must hold the project publication barrier and invalidate
    /// all checkpoint state IDs before calling.
    pub fn validate_published(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
    ) -> Result<ProofState> {
        self.validate_published_with_progress(project, actor, target, |_, _| {})
    }

    /// [`Self::validate_published`] with synchronous, read-only phase hooks.
    /// Hooks report stable phase names and bounded summaries; they never own
    /// lifecycle state and may be used by MCP's independent polling record.
    pub fn validate_published_with_progress(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        mut progress: impl FnMut(&'static str, &str),
    ) -> Result<ProofState> {
        // Design note: a PET-finished declaration is still only a snapshot of
        // the bytes PET inspected during `prove`. Validate that snapshot
        // before running Dune; otherwise an external editor could replace an
        // admitted/closed declaration between resolution and the native build
        // and we would report the replacement as the requested theorem.
        self.validate_target(project, target)?;
        writeback::build_and_refresh(
            project,
            actor,
            &target.anchor.source,
            self.config.command_timeout,
            &mut progress,
            |project, actor| {
                // The build runs outside Rust's source CAS.  Check again
                // after it returns so an editor racing the build cannot make
                // a different declaration appear completed.
                self.validate_target(project, target)?;
                let resolved = self.resolve(project, actor, &target.info.identity)?;
                if !resolved.proof_finished {
                    return Err(Error::new(
                        ErrorKind::InvalidDeclaration,
                        "built declaration is not a completed proof",
                    ));
                }
                let current = resolved.target.ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidDeclaration,
                        "built declaration disappeared",
                    )
                })?;
                self.audit_target(project, actor, &current)?;
                Ok(completed_state(current.info))
            },
        )
    }

    /// Validate PET's finalizer while the current epoch is still live. The MCP
    /// coordinator invalidates every checkpoint only after this succeeds.
    pub fn close_proof(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        final_state: pet::PetStateId,
    ) -> Result<()> {
        self.validate_target(project, target)?;
        let closed = actor
            .run(final_state, target.info.kind.terminator())
            .map_err(finalizer_pet_error)?;
        if closed.goals.proof_mode || !closed.proof_finished {
            actor
                .release_states(&[closed.state])
                .map_err(finalizer_pet_error)?;
            return Err(Error::new(
                ErrorKind::ProofStepFailed,
                "the generated proof terminator did not close the proof",
            ));
        }
        actor
            .release_states(&[closed.state])
            .map_err(finalizer_pet_error)
    }

    /// Publish one already-finalized linear MCP checkpoint path. The caller
    /// holds the project barrier and has invalidated all pre-write state IDs.
    pub fn publish(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        fragments: &[String],
        begin_epoch: impl FnOnce(),
    ) -> Result<ProofState> {
        self.publish_with_progress(project, actor, target, fragments, begin_epoch, |_, _| {})
    }

    /// [`Self::publish`] with pollable phase hooks.  Progress callbacks are
    /// observational only and run while the caller already owns the project
    /// publication barrier.
    pub fn publish_with_progress(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        fragments: &[String],
        begin_epoch: impl FnOnce(),
        mut progress: impl FnMut(&'static str, &str),
    ) -> Result<ProofState> {
        self.validate_target(project, target)?;
        progress("prepare_writeback", "rendering the source transaction");
        let publication = writeback::prepare(project, target, fragments)?;
        begin_epoch();
        writeback::publish_with_progress(
            project,
            actor,
            publication,
            self.config.command_timeout,
            &mut progress,
            |project, actor| {
                let resolved = self.resolve(project, actor, &target.info.identity)?;
                if !resolved.proof_finished {
                    return Err(Error::new(
                        ErrorKind::InvalidDeclaration,
                        "written declaration is not a completed proof",
                    ));
                }
                let current = resolved.target.ok_or_else(|| {
                    Error::new(
                        ErrorKind::InvalidDeclaration,
                        "written declaration disappeared",
                    )
                })?;
                if current.info.identity != target.info.identity
                    || current.info.kind != target.info.kind
                {
                    return Err(Error::new(
                        ErrorKind::DeclarationChanged,
                        "written declaration identity changed",
                    ));
                }
                self.audit_target(project, actor, &current)?;
                Ok(completed_state(current.info))
            },
        )
    }

    pub fn validate_target(&self, project: &DuneProject, target: &DeclarationTarget) -> Result<()> {
        let selected = project
            .source(&target.info.identity.file)
            .map_err(|error| {
                if error.kind == ErrorKind::NotFound {
                    Error::new(
                        ErrorKind::DeclarationChanged,
                        "Dune no longer selects the proof's source file",
                    )
                } else {
                    error
                }
            })?;
        if selected != target.anchor.source {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "Dune source selection changed",
            ));
        }
        let bytes = fs::read(&selected).map_err(|_| {
            Error::new(
                ErrorKind::DeclarationChanged,
                "target source is unavailable",
            )
        })?;
        if <[u8; 32]>::from(Sha256::digest(bytes)) != target.anchor.digest {
            return Err(Error::new(
                ErrorKind::DeclarationChanged,
                "target source changed while proof was open",
            ));
        }
        Ok(())
    }

    fn open_target(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: DeclarationTarget,
    ) -> Result<OpenedProof> {
        let workspace = project.pet_workspace(&target.anchor.source)?;
        let pet_source = project.pet_source(&target.anchor.source)?;
        let opened = actor
            .state_at(&workspace, &pet_source, target.anchor.header.end)
            .map_err(declaration_pet_error)?;
        if let Err(error) = require_open_proof(&opened) {
            actor
                .release_states(&[opened.state])
                .map_err(declaration_pet_error)?;
            return Err(error);
        }
        Ok(OpenedProof {
            state: opened.state,
            view: proof_state(target.info.clone(), &opened.goals),
            finished: opened.proof_finished,
            target,
        })
    }

    fn declarations(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        file: &FileId,
    ) -> Result<Vec<ResolvedDeclaration>> {
        let source = project.source(file)?;
        let workspace = project.pet_workspace(&source)?;
        let pet_source = project.pet_source(&source)?;
        let rows = actor
            .document_declarations(&workspace, &pet_source)
            .map_err(declaration_pet_error)?;
        let bytes = fs::read(&source).map_err(|_| {
            Error::new(
                ErrorKind::DeclarationChanged,
                "declaration source is unavailable",
            )
        })?;
        let digest = Sha256::digest(&bytes).into();
        let library = project.library(&source)?;
        for row in &rows {
            if row.qualified_path.is_empty()
                || row.range.start >= row.range.end
                || row.declaration_range.start != row.range.start
                || row.range.end > row.declaration_range.end
                || row.declaration_range.end > bytes.len()
            {
                return Err(Error::new(
                    ErrorKind::InvalidConfiguration,
                    "declaration metadata is inconsistent with the selected source bytes",
                ));
            }
        }
        let replaceable = replaceable_declaration_ranges(&rows);
        let mut identities = BTreeSet::new();
        rows.into_iter()
            .enumerate()
            .map(|(index, row)| {
                if !row.qualified_path.starts_with(&library.0)
                    || row.qualified_path.len() <= library.0.len()
                {
                    return Err(Error::new(
                        ErrorKind::InvalidConfiguration,
                        format!(
                            "declaration path '{}' disagrees with Dune compilation unit '{}'",
                            row.qualified_path.join("."),
                            library.0.join("."),
                        ),
                    ));
                }
                let identity = DeclarationIdentity {
                    file: file.clone(),
                    qualified_path: row.qualified_path,
                };
                if !identities.insert(identity.clone()) {
                    return Err(Error::new(
                        ErrorKind::Ambiguous,
                        "declaration identity is ambiguous",
                    ));
                }
                let kind = declaration_kind(&row.kind);
                let target = kind.map(|kind| DeclarationTarget {
                    info: DeclarationInfo {
                        identity: identity.clone(),
                        kind,
                        statement: row.statement,
                    },
                    anchor: SourceAnchor {
                        source: source.clone(),
                        digest,
                        header: PetRange {
                            start: row.range.start,
                            end: row.range.end,
                        },
                        declaration: PetRange {
                            start: row.declaration_range.start,
                            end: row.declaration_range.end,
                        },
                        replaceable: replaceable[index],
                    },
                    new_header: None,
                });
                Ok(ResolvedDeclaration {
                    identity,
                    target,
                    proof_finished: row.proof_finished,
                    explicit_axiom: row.kind == "Axiom",
                })
            })
            .collect()
    }

    fn resolve(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        identity: &DeclarationIdentity,
    ) -> Result<ResolvedDeclaration> {
        self.declarations(project, actor, &identity.file)?
            .into_iter()
            .find(|candidate| &candidate.identity == identity)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    "declaration is not present in the selected source context",
                )
            })
    }

    fn audit_target(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
    ) -> Result<()> {
        let workspace = project.pet_workspace(&target.anchor.source)?;
        let bytes = fs::read(&target.anchor.source).map_err(|_| {
            Error::new(ErrorKind::DeclarationChanged, "audit source is unavailable")
        })?;
        let context = actor
            .state_at(
                &workspace,
                &project.pet_source(&target.anchor.source)?,
                bytes.len(),
            )
            .map_err(query_pet_error)?;
        let result = self.audit_state(project, actor, target, context.state);
        let released = actor
            .release_states(&[context.state])
            .map_err(query_pet_error);
        result.and(released)
    }

    fn audit_state(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        state: pet::PetStateId,
    ) -> Result<()> {
        let report = actor
            .assumptions(state, &target.info.identity.qualified_path)
            .map_err(query_pet_error)?;
        let mut unsafe_theory = Vec::new();
        if report.theory.rewrite_rules {
            unsafe_theory.push("rewrite rules");
        }
        if report.theory.impredicative_set {
            unsafe_theory.push("impredicative Set");
        }
        if report.theory.type_in_type {
            unsafe_theory.push("type-in-type");
        }
        if !unsafe_theory.is_empty() {
            return Err(Error::new(
                ErrorKind::AxiomDependencyOutOfScope,
                format!(
                    "proof relies on unsafe Rocq theory features: {}",
                    unsafe_theory.join(", ")
                ),
            ));
        }
        for assumption in report.assumptions {
            let canonical = assumption.qualified_path.join(".");
            if assumption.kind != pet::PetAssumptionKind::Axiom {
                let kind = match assumption.kind {
                    pet::PetAssumptionKind::Axiom => unreachable!(),
                    pet::PetAssumptionKind::Positive => "unchecked positivity",
                    pet::PetAssumptionKind::Guarded => "unchecked guardedness",
                    pet::PetAssumptionKind::TypeInType => "unsafe universe hierarchy",
                    pet::PetAssumptionKind::Uip => "definitional UIP",
                    pet::PetAssumptionKind::SectionVariable => "section variable",
                    pet::PetAssumptionKind::Opaque => "opaque constant",
                    pet::PetAssumptionKind::Transparent => "transparent constant",
                };
                return Err(Error::new(
                    ErrorKind::AxiomDependencyOutOfScope,
                    format!("proof relies on {kind} '{canonical}'"),
                ));
            }
            let source = project.source_for_constant(&canonical).ok_or_else(|| {
                Error::new(
                    ErrorKind::AxiomDependencyOutOfScope,
                    format!("axiom dependency is outside the Dune project: {canonical}"),
                )
            })?;
            let file = FileId::from_path(&project.root, &source)
                .map_err(|message| Error::new(ErrorKind::InvalidConfiguration, message))?;
            let declaration = self
                .declarations(project, actor, &file)?
                .into_iter()
                .find(|candidate| candidate.identity.qualified_name() == canonical)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::AxiomDependencyOutOfScope,
                        format!("could not classify axiom dependency: {canonical}"),
                    )
                })?;
            if !declaration.explicit_axiom {
                return Err(Error::new(
                    ErrorKind::UnfinishedDependency,
                    format!("proof depends on unfinished declaration '{canonical}'"),
                ));
            }
        }
        Ok(())
    }
}

struct ResolvedDeclaration {
    identity: DeclarationIdentity,
    target: Option<DeclarationTarget>,
    proof_finished: bool,
    explicit_axiom: bool,
}

/// Enrich a PET missing-reference rejection from a synthetic declaration
/// header.  PET's byte range is authoritative; the wrapper only translates the
/// fixed `"Theorem name : "` prefix and then asks the project-wide PET-backed
/// index for candidates.  If lookup itself fails, the original Rocq error is
/// returned unchanged.
fn enrich_declaration_error(
    engine: &Engine,
    project: &DuneProject,
    actor: &pet::PetActor,
    mut error: Error,
    statement: &str,
    kind: DeclarationKind,
    leaf: &str,
) -> Error {
    if error.resolution.is_none() {
        return error;
    }
    let prefix = format!("{} {} : ", kind.keyword(), leaf).len();
    let statement_len = statement.len();
    let translated = error.diagnostic.as_ref().and_then(|diagnostic| {
        let start = diagnostic.byte_start.checked_sub(prefix)?;
        let end = diagnostic.byte_end.checked_sub(prefix)?;
        (start <= end
            && end <= statement_len
            && statement.is_char_boundary(start)
            && statement.is_char_boundary(end))
        .then_some(ProofDiagnostic {
            byte_start: start,
            byte_end: end,
        })
    });
    let probe = translated
        .as_ref()
        .and_then(|range| statement.get(range.byte_start..range.byte_end))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let candidates = if probe.is_empty() {
        Vec::new()
    } else {
        engine
            .locate_symbols(project, actor, probe, 20)
            .unwrap_or_default()
    };
    let mut imports = candidates
        .iter()
        .map(|candidate| candidate.require_import.clone())
        .collect::<Vec<_>>();
    imports.sort();
    imports.dedup();
    let candidate_json = candidates
        .iter()
        .map(|candidate| {
            serde_json::json!({
                "id": {
                    "file": candidate.identity.file.0,
                    "qualified_path": candidate.identity.qualified_path,
                },
                "qualified_name": candidate.qualified_name,
                "kind": format!("{:?}", candidate.kind),
                "statement": candidate.statement,
                "require_import": candidate.require_import,
                "match": candidate.match_kind,
            })
        })
        .collect::<Vec<_>>();
    let mut resolution = serde_json::json!({
        "kind": "missing_identifier",
        "note": "the declaration statement environment lacks an import for this identifier",
        "identifier": probe,
        "suggested_imports": imports,
        "candidates": candidate_json,
    });
    if let Some(range) = translated {
        resolution["statement_byte_range"] = serde_json::json!({
            "start": range.byte_start,
            "end": range.byte_end,
        });
        // The translated range is more useful to callers than the synthetic
        // header range, while the latter remains available for raw debugging.
        error.diagnostic = Some(range);
    }
    error.resolution = Some(resolution);
    error
}

/// Classify PET ranges structurally without interpreting Rocq source. A range
/// can be replaced only when no other declaration record intersects it.
fn replaceable_declaration_ranges(rows: &[pet::PetDeclaration]) -> Vec<bool> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            !rows.iter().enumerate().any(|(other_index, other)| {
                index != other_index
                    && row.declaration_range.start < other.declaration_range.end
                    && other.declaration_range.start < row.declaration_range.end
            })
        })
        .collect()
}

/// Validate the non-semantic shape of a public declaration ID. PET remains the
/// authority for whether its components are valid Rocq identifiers.
pub fn validate_identity(identity: &DeclarationIdentity) -> Result<()> {
    validate_file_id(&identity.file)?;
    if identity.qualified_path.is_empty()
        || identity.qualified_path.iter().any(|part| part.is_empty())
    {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration path is empty",
        ));
    }
    Ok(())
}

fn validate_file_id(file: &FileId) -> Result<()> {
    let path = Path::new(&file.0);
    if file.0.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "file must be a safe project-relative path",
        ));
    }
    Ok(())
}

fn relative_path<'a>(
    identity: &'a DeclarationIdentity,
    library: &LogicalLibrary,
) -> Result<&'a [String]> {
    if !identity.qualified_path.starts_with(&library.0)
        || identity.qualified_path.len() <= library.0.len()
    {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "declaration path is outside its Dune logical library",
        ));
    }
    Ok(&identity.qualified_path[library.0.len()..])
}

/// Form one declaration identity from Dune's compilation unit and a caller
/// name that is strictly relative to it. Repeating Dune's prefix is rejected
/// rather than normalized, so there is exactly one owner and one input form.
fn declared_identity(
    file: FileId,
    library: &LogicalLibrary,
    name: &str,
) -> Result<DeclarationIdentity> {
    let parts = name.split('.').map(str::to_owned).collect::<Vec<_>>();
    if parts.iter().any(String::is_empty) {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "name contains an empty component",
        ));
    }
    if parts.starts_with(&library.0) {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "name must not repeat its Dune compilation-unit prefix",
        ));
    }
    // Design note: every dotted input is a local nested-module path. Dune is
    // the only source of the compilation-unit prefix prepended below.
    Ok(DeclarationIdentity {
        file,
        qualified_path: library.0.iter().cloned().chain(parts).collect(),
    })
}

fn declaration_kind(value: &str) -> Option<DeclarationKind> {
    match value {
        "Theorem" => Some(DeclarationKind::Theorem),
        "Lemma" => Some(DeclarationKind::Lemma),
        "Fact" => Some(DeclarationKind::Fact),
        "Remark" => Some(DeclarationKind::Remark),
        "Corollary" => Some(DeclarationKind::Corollary),
        "Proposition" => Some(DeclarationKind::Proposition),
        "Definition" => Some(DeclarationKind::Definition),
        _ => None,
    }
}

fn validate_fragment(fragment: &str) -> Result<()> {
    if fragment.trim().is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "proof fragment is empty",
        ));
    }
    if fragment.len() > 1024 * 1024 {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "proof fragment exceeds 1 MiB",
        ));
    }
    Ok(())
}

pub fn validate_fragments(fragments: &[String]) -> Result<()> {
    if fragments.is_empty() || fragments.len() > 20 {
        return Err(Error::new(
            ErrorKind::InvalidRequest,
            "attempts must contain 1 to 20 fragments",
        ));
    }
    for fragment in fragments {
        validate_fragment(fragment)?;
    }
    Ok(())
}

fn require_open_proof(execution: &pet::PetExecution) -> Result<()> {
    if !execution.goals.proof_mode || !execution.goals.given_up.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidDeclaration,
            "the declaration did not open a safe proof state",
        ));
    }
    Ok(())
}

fn checked_trace_success(
    actor: &pet::PetActor,
    target: &DeclarationTarget,
    execution: pet::PetExecution,
) -> Result<TracedProofStep> {
    if !execution.goals.proof_mode || !execution.goals.given_up.is_empty() {
        actor
            .release_states(&[execution.state])
            .map_err(step_pet_error)?;
        return Err(Error::new(
            ErrorKind::ProofStepFailed,
            "fragment closes proof mode, runs a global command, or gives up a goal",
        ));
    }
    let mut view = proof_state(target.info.clone(), &execution.goals);
    view.structured_goals = Some(structured_goals(&execution.goals));
    Ok(TracedProofStep::Success(ProofStep {
        state: execution.state,
        view,
        finished: execution.proof_finished,
    }))
}

fn proof_state(info: DeclarationInfo, goals: &pet::PetGoals) -> ProofState {
    let rendered = render_goals(goals, GoalScope::Focused, None)
        .expect("rendering all focused PET goals cannot miss an id");
    proof_state_with_rendering(info, goals, rendered)
}

fn proof_state_with_rendering(
    info: DeclarationInfo,
    goals: &pet::PetGoals,
    rendered: String,
) -> ProofState {
    ProofState {
        theorem: info,
        lifecycle: ProofLifecycle::Open,
        goals: rendered,
        goal_focus: goals.focus(),
        structured_goals: None,
    }
}

/// Materialize PET's structured goal contexts only at an explicit query or
/// diagnostic boundary.  The conversion is lossless with respect to PET's
/// protocol fields and does not infer anything from the pretty-printed text.
fn structured_goals(goals: &pet::PetGoals) -> StructuredGoals {
    fn one(goal: &pet::PetGoal) -> GoalDetail {
        GoalDetail {
            evar: goal.evar.clone(),
            name: goal.name.clone(),
            hypotheses: goal
                .hypotheses
                .iter()
                .map(|hypothesis| GoalHypothesis {
                    names: hypothesis.names.clone(),
                    definition: hypothesis.definition.clone(),
                    ty: hypothesis.ty.clone(),
                })
                .collect(),
            ty: goal.ty.clone(),
        }
    }
    let stack = goals
        .stack
        .iter()
        .map(|frame| GoalStackDetail {
            left: frame.left.iter().map(one).collect(),
            right: frame.right.iter().map(one).collect(),
        })
        .collect::<Vec<_>>();
    let unfocused = stack
        .iter()
        .flat_map(|frame| frame.left.iter().chain(frame.right.iter()))
        .cloned()
        .collect();
    StructuredGoals {
        focused: goals.focused.iter().map(one).collect(),
        stack,
        unfocused,
        shelved: goals.shelved.iter().map(one).collect(),
        given_up: goals.given_up.iter().map(one).collect(),
        next_bullet: goals.bullet.clone(),
    }
}

/// Return an operation result only after its temporary PET state has been
/// released. A release/transport failure takes precedence because every other
/// state in that process must then be invalidated by the MCP coordinator.
fn release_after<T>(actor: &pet::PetActor, state: pet::PetStateId, result: Result<T>) -> Result<T> {
    let released = actor
        .release_states(&[state])
        .map_err(declaration_pet_error);
    match (result, released) {
        (_, Err(error)) => Err(error),
        (result, Ok(())) => result,
    }
}

fn completed_state(info: DeclarationInfo) -> ProofState {
    ProofState {
        theorem: info,
        lifecycle: ProofLifecycle::Completed,
        goals: String::new(),
        goal_focus: GoalFocus::default(),
        structured_goals: None,
    }
}

fn match_rank(kind: &str) -> u8 {
    match kind {
        "exact" => 0,
        "suffix" => 1,
        "leaf" => 2,
        _ => 3,
    }
}

/// Classify one canonical name against a user query without treating a bare
/// leaf as a suffix. Dotted queries are intentionally suffix-only unless they
/// match the complete qualified name exactly.
fn symbol_match_kind(qualified_name: &str, leaf: &str, symbol: &str) -> Option<&'static str> {
    if qualified_name == symbol {
        Some("exact")
    } else if symbol.contains('.') && qualified_name.ends_with(&format!(".{symbol}")) {
        Some("suffix")
    } else if !symbol.contains('.') && leaf == symbol {
        Some("leaf")
    } else {
        None
    }
}

/// Read one compiler-produced glob index. A match is only a candidate hint;
/// PET rechecks the source declaration. A source newer than its glob returns
/// None, forcing a conservative source-text fallback for that file.
fn glob_matches(_source: &Path, glob: &Path, symbol: &str) -> Option<bool> {
    let source = _source;
    let source_mtime = fs::metadata(source).ok()?.modified().ok()?;
    let glob_mtime = fs::metadata(glob).ok()?.modified().ok()?;
    if source_mtime > glob_mtime {
        return None;
    }
    let text = fs::read_to_string(glob).ok()?;
    let mut lines = text.lines();
    let _digest_line = lines.next()?;
    let prefix = lines.next()?.strip_prefix('F')?.to_owned();
    let leaf = symbol.rsplit('.').next().unwrap_or(symbol);
    for line in lines {
        if line.is_empty() || line.starts_with('R') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let _kind = fields.next();
        let _range = fields.next();
        let section = fields.next();
        let name = fields.next();
        let (Some(section), Some(name)) = (section, name) else {
            continue;
        };
        let section = (section != "<>").then_some(section);
        let qualified = match section {
            Some(section) => format!("{prefix}.{section}.{name}"),
            None => format!("{prefix}.{name}"),
        };
        if symbol_match_kind(&qualified, name, symbol).is_some()
            || (symbol.contains('.')
                && (qualified.ends_with(&format!(".{symbol}")) || name == leaf))
            || (!symbol.contains('.') && name == leaf)
        {
            return Some(true);
        }
    }
    Some(false)
}

/// Conservative source-text hint used only when a `.glob` index is missing or
/// stale.  Identifier boundaries avoid turning a common substring (for
/// example `soundness` in `unsoundness`) into a false candidate, while PET
/// remains the final authority for declaration identity.
fn contains_identifier(source: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut offset = 0usize;
    while let Some(relative) = source[offset..].find(needle) {
        let start = offset + relative;
        let end = start + needle.len();
        let before = source[..start].chars().next_back();
        let after = source[end..].chars().next();
        let identifier = |character: Option<char>| {
            character.is_some_and(|character| {
                character == '_' || character == '\'' || character.is_alphanumeric()
            })
        };
        if !identifier(before) && !identifier(after) {
            return true;
        }
        offset = end;
        if offset >= source.len() {
            break;
        }
    }
    false
}

/// Render PET's structured goals without interpreting Rocq syntax.
///
/// `scope` chooses which PET-owned collections participate. If `goal_id` is
/// present, exactly the matching goal in that scope is rendered; absence is
/// reported with `None`. The wrapper preserves the historical focused-goal
/// text format and never derives proof completion from this projection.
fn render_goals(
    goals: &pet::PetGoals,
    scope: GoalScope,
    goal_id: Option<&[serde_json::Value]>,
) -> Option<String> {
    let mut selected = Vec::new();
    if matches!(scope, GoalScope::Focused | GoalScope::All) {
        selected.extend(goals.focused.iter());
    }
    if matches!(scope, GoalScope::Unfocused | GoalScope::All) {
        for frame in &goals.stack {
            selected.extend(frame.left.iter());
            selected.extend(frame.right.iter());
        }
    }
    if matches!(scope, GoalScope::Shelved | GoalScope::All) {
        selected.extend(goals.shelved.iter());
    }
    if matches!(scope, GoalScope::GivenUp | GoalScope::All) {
        selected.extend(goals.given_up.iter());
    }
    if let Some(goal_id) = goal_id {
        let goal = selected
            .into_iter()
            .find(|goal| goal.evar.as_slice() == goal_id)?;
        return Some(render_goal(goal, 0));
    }
    Some(
        selected
            .into_iter()
            .enumerate()
            .map(|(index, goal)| render_goal(goal, index))
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

fn render_goal(goal: &pet::PetGoal, index: usize) -> String {
    let mut lines = Vec::new();
    if let Some(name) = &goal.name {
        lines.push(format!("goal {} ({name})", index + 1));
    }
    for hypothesis in &goal.hypotheses {
        let names = hypothesis.names.join(" ");
        match &hypothesis.definition {
            Some(value) => lines.push(format!("{names} := {value} : {}", hypothesis.ty)),
            None => lines.push(format!("{names} : {}", hypothesis.ty)),
        }
    }
    lines.push("============================".into());
    lines.push(goal.ty.clone());
    lines.join("\n")
}

/// Public-facing operation context for projecting a PET remote error.  PET's
/// numeric code is operation-independent; this small type is the sole place
/// where a semantic rejection becomes an MCP error class.
#[derive(Clone, Copy)]
enum PetOperation {
    Declaration,
    ProofStep,
    Query,
}

/// Project one typed PET remote rejection according to the operation that
/// produced it.
///
/// Design note: keeping this table in one place prevents a new PET code from
/// being accidentally classified as a project configuration failure in one
/// path and a user proof error in another.
fn remote_pet_kind(kind: pet::PetRemoteKind, operation: PetOperation) -> ErrorKind {
    use pet::PetRemoteKind::*;
    match (operation, kind) {
        (_, MethodNotFound) => ErrorKind::InvalidConfiguration,
        (_, Anomaly | System | Unknown(_)) => ErrorKind::PetFailure,
        (PetOperation::Query, TheoremNotFound | NoNodeAtPoint | ReferenceNotFound) => {
            ErrorKind::NotFound
        }
        (PetOperation::Query, Interrupted | Parsing | Coq) => ErrorKind::QueryFailed,
        (PetOperation::Declaration, TheoremNotFound | NoNodeAtPoint) => ErrorKind::NotFound,
        (PetOperation::Declaration, Interrupted | Parsing | Coq | ReferenceNotFound) => {
            ErrorKind::InvalidDeclaration
        }
        (PetOperation::ProofStep, TheoremNotFound | NoNodeAtPoint) => ErrorKind::NotFound,
        (PetOperation::ProofStep, Interrupted | Parsing | Coq | ReferenceNotFound) => {
            ErrorKind::ProofStepFailed
        }
    }
}

fn declaration_pet_error(error: pet::PetError) -> Error {
    let missing_reference = matches!(
        &error,
        pet::PetError::Remote {
            kind: pet::PetRemoteKind::ReferenceNotFound,
            ..
        }
    );
    let kind = match &error {
        pet::PetError::Cancelled => ErrorKind::RequestCancelled,
        pet::PetError::TimedOut { .. } => ErrorKind::ProofStepTimeout,
        error if error.lost() => ErrorKind::PetLost,
        pet::PetError::Environment(_) => ErrorKind::InvalidConfiguration,
        pet::PetError::Invalid(_) => ErrorKind::InvalidDeclaration,
        pet::PetError::Remote { kind, .. } => remote_pet_kind(*kind, PetOperation::Declaration),
        pet::PetError::Protocol(_) | pet::PetError::OutputOverflow => ErrorKind::PetLost,
        pet::PetError::ProcessLost(_) => ErrorKind::PetLost,
    };
    // Declaration headers are synthetic fragments, but PET still gives an
    // exact range.  Preserve it so the caller can translate it back to the
    // submitted statement and attach import guidance.
    let projected = projected_pet_error(kind, error, true);
    if missing_reference {
        projected.with_resolution(serde_json::json!({
            "kind": "missing_identifier",
            "note": "the declaration statement environment lacks an import for this identifier",
            "suggested_imports": [],
            "candidates": [],
        }))
    } else {
        projected
    }
}

fn step_pet_error(error: pet::PetError) -> Error {
    let kind = match &error {
        pet::PetError::Cancelled => ErrorKind::RequestCancelled,
        pet::PetError::TimedOut { .. } => ErrorKind::ProofStepTimeout,
        error if error.lost() => ErrorKind::PetLost,
        pet::PetError::Environment(_) => ErrorKind::InvalidConfiguration,
        pet::PetError::Invalid(_) => ErrorKind::InvalidRequest,
        pet::PetError::Remote { kind, .. } => remote_pet_kind(*kind, PetOperation::ProofStep),
        pet::PetError::Protocol(_) | pet::PetError::OutputOverflow => ErrorKind::PetLost,
        pet::PetError::ProcessLost(_) => ErrorKind::PetLost,
    };
    projected_pet_error(kind, error, true)
}

/// The closing command is generated by the wrapper rather than supplied by
/// the caller. Preserve its typed failure class/message but do not mislabel a
/// `Qed.`/`Defined.` range as a location in the accepted proof fragment.
fn finalizer_pet_error(error: pet::PetError) -> Error {
    let mut projected = step_pet_error(error);
    projected.diagnostic = None;
    projected
}

fn query_pet_error(error: pet::PetError) -> Error {
    let kind = match &error {
        pet::PetError::Cancelled => ErrorKind::RequestCancelled,
        pet::PetError::TimedOut { .. } => ErrorKind::ProofStepTimeout,
        error if error.lost() => ErrorKind::PetLost,
        pet::PetError::Invalid(_) => ErrorKind::InvalidRequest,
        pet::PetError::Environment(_) => ErrorKind::InvalidConfiguration,
        pet::PetError::Remote { kind, .. } => remote_pet_kind(*kind, PetOperation::Query),
        pet::PetError::Protocol(_) | pet::PetError::OutputOverflow => ErrorKind::PetLost,
        pet::PetError::ProcessLost(_) => ErrorKind::PetLost,
    };
    projected_pet_error(kind, error, false)
}

fn projected_pet_error(kind: ErrorKind, error: pet::PetError, preserve_diagnostic: bool) -> Error {
    let semantic = error.is_semantic();
    let diagnostic = preserve_diagnostic
        .then(|| match &error {
            pet::PetError::Remote { diagnostic, .. } => diagnostic.clone(),
            _ => None,
        })
        .flatten();
    let result = Error::new(kind, error.public_message());
    let result = if semantic { result.semantic() } else { result };
    match diagnostic {
        Some(diagnostic) => result.with_diagnostic(diagnostic),
        None => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_identity_accepts_only_compilation_unit_relative_names() {
        let library = LogicalLibrary(vec!["Demo".into(), "Main".into()]);
        let file = FileId("Main.v".into());
        for (name, expected) in [
            ("fresh", vec!["Demo", "Main", "fresh"]),
            ("Nested.fresh", vec!["Demo", "Main", "Nested", "fresh"]),
        ] {
            let identity = declared_identity(file.clone(), &library, name).unwrap();
            assert_eq!(identity.qualified_path, expected);
        }
        for name in ["Demo.Main.fresh", "Demo.Main", "Nested..fresh"] {
            assert_eq!(
                declared_identity(file.clone(), &library, name)
                    .unwrap_err()
                    .kind,
                ErrorKind::InvalidDeclaration
            );
        }
    }

    fn remote(kind: pet::PetRemoteKind) -> pet::PetError {
        pet::PetError::Remote {
            code: -32000,
            kind,
            message: "typed diagnostic".into(),
            diagnostic: None,
        }
    }

    #[test]
    fn pet_remote_errors_keep_operation_semantics() {
        assert!(query_pet_error(remote(pet::PetRemoteKind::ReferenceNotFound)).semantic);
        assert!(step_pet_error(remote(pet::PetRemoteKind::Coq)).semantic);
        assert!(!query_pet_error(remote(pet::PetRemoteKind::Anomaly)).semantic);
        assert_eq!(
            query_pet_error(remote(pet::PetRemoteKind::ReferenceNotFound)).kind,
            ErrorKind::NotFound
        );
        assert_eq!(
            query_pet_error(remote(pet::PetRemoteKind::Coq)).kind,
            ErrorKind::QueryFailed
        );
        assert_eq!(
            query_pet_error(remote(pet::PetRemoteKind::Anomaly)).kind,
            ErrorKind::PetFailure
        );
        assert_eq!(
            declaration_pet_error(remote(pet::PetRemoteKind::ReferenceNotFound)).kind,
            ErrorKind::InvalidDeclaration
        );
        assert_eq!(
            step_pet_error(remote(pet::PetRemoteKind::ReferenceNotFound)).kind,
            ErrorKind::ProofStepFailed
        );
        assert!(
            step_pet_error(remote(pet::PetRemoteKind::ReferenceNotFound))
                .resolution
                .is_none()
        );
        assert!(
            declaration_pet_error(remote(pet::PetRemoteKind::ReferenceNotFound))
                .resolution
                .is_some()
        );
        assert_eq!(
            declaration_pet_error(remote(pet::PetRemoteKind::MethodNotFound)).kind,
            ErrorKind::InvalidConfiguration
        );
    }

    fn declaration(name: &str, start: usize, end: usize) -> pet::PetDeclaration {
        pet::PetDeclaration {
            qualified_path: vec!["Demo".into(), name.into()],
            kind: "Definition".into(),
            range: pet::PetRange { start, end },
            declaration_range: pet::PetRange { start, end },
            proof_finished: true,
            statement: format!("Definition {name} := 0"),
        }
    }

    #[test]
    fn overlapping_pet_ranges_are_not_individually_replaceable() {
        let rows = [
            declaration("first", 0, 20),
            declaration("second", 0, 20),
            declaration("third", 20, 30),
            declaration("fourth", 25, 40),
        ];
        assert_eq!(
            replaceable_declaration_ranges(&rows),
            vec![false, false, false, false]
        );
        assert_eq!(
            replaceable_declaration_ranges(&[declaration("only", 0, 20)]),
            vec![true]
        );
    }

    #[test]
    fn symbol_matching_distinguishes_exact_suffix_and_leaf_queries() {
        assert_eq!(
            symbol_match_kind("Demo.A.map", "map", "Demo.A.map"),
            Some("exact")
        );
        assert_eq!(
            symbol_match_kind("Demo.A.map", "map", "A.map"),
            Some("suffix")
        );
        assert_eq!(symbol_match_kind("Demo.A.map", "map", "map"), Some("leaf"));
        assert_eq!(symbol_match_kind("Demo.A.map", "map", "other"), None);
        // A bare name is a leaf query, not a suffix query; this keeps the
        // ranking observable and prevents the leaf class from becoming dead.
        assert_eq!(symbol_match_kind("Demo.A.map", "map", "map"), Some("leaf"));
    }

    fn goal(id: i64, ty: &str) -> pet::PetGoal {
        pet::PetGoal {
            evar: vec![serde_json::json!("Ser_Evar"), serde_json::json!(id)],
            name: None,
            hypotheses: Vec::new(),
            ty: ty.into(),
        }
    }

    #[test]
    fn goal_rendering_selects_pet_scopes_and_exact_evar_ids() {
        let goals = pet::PetGoals {
            focused: vec![goal(1, "focused")],
            stack: vec![pet::PetGoalStackFrame {
                left: vec![goal(2, "left")],
                right: vec![goal(3, "right")],
            }],
            shelved: vec![goal(4, "shelved")],
            given_up: vec![goal(5, "given-up")],
            bullet: Some("Focus next goal with bullet -.".into()),
            proof_mode: true,
        };
        assert_eq!(
            render_goals(&goals, GoalScope::Focused, None).unwrap(),
            "============================\nfocused"
        );
        let unfocused = render_goals(&goals, GoalScope::Unfocused, None).unwrap();
        assert!(unfocused.contains("left"));
        assert!(unfocused.contains("right"));
        assert!(!unfocused.contains("focused"));
        assert_eq!(
            render_goals(
                &goals,
                GoalScope::All,
                Some(&[serde_json::json!("Ser_Evar"), serde_json::json!(4)]),
            )
            .unwrap(),
            "============================\nshelved"
        );
        assert!(
            render_goals(
                &goals,
                GoalScope::Focused,
                Some(&[serde_json::json!("Ser_Evar"), serde_json::json!(4)]),
            )
            .is_none()
        );
    }

    #[test]
    fn pet_run_error_preserves_unicode_fragment_byte_range() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("dune-project"),
            "(lang dune 3.22)\n(using rocq 0.12)\n",
        )
        .unwrap();
        fs::write(directory.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
        fs::write(
            directory.path().join("A.v"),
            "Theorem target : True.\nProof.\nAdmitted.\n",
        )
        .unwrap();
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let project = engine.attach(directory.path()).unwrap();
        let actor = pet::PetActor::new();
        engine
            .reconcile_source(&project, &FileId("A.v".into()))
            .unwrap();
        let info = engine
            .list_decls(&project, &actor, &FileId("A.v".into()))
            .unwrap()
            .into_iter()
            .find(|info| info.identity.qualified_path.last().unwrap() == "target")
            .unwrap();
        let opened = match engine
            .open_declaration(&project, &actor, info.identity)
            .unwrap()
        {
            OpenResult::Open(opened) => *opened,
            OpenResult::Published(_) => panic!("admitted source was reported as completed"),
        };
        let error = engine
            .run(
                &project,
                &actor,
                &opened.target,
                opened.state,
                "idtac \"🦀\". nonsense.",
                None,
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::ProofStepFailed);
        assert_eq!(
            error.diagnostic,
            Some(ProofDiagnostic {
                byte_start: 14,
                byte_end: 22,
            })
        );
        actor.release_states(&[opened.state]).unwrap();
    }

    #[test]
    fn pet_compound_command_is_discoverable_but_not_replaceable() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("dune-project"),
            "(lang dune 3.22)\n(using rocq 0.12)\n",
        )
        .unwrap();
        fs::write(directory.path().join("dune"), "(rocq.theory (name Demo))\n").unwrap();
        fs::write(
            directory.path().join("A.v"),
            "Inductive even : nat -> Prop :=\n\
             | even_O : even 0\n\
             | even_S : forall n, odd n -> even (S n)\n\
             with odd : nat -> Prop :=\n\
             | odd_S : forall n, even n -> odd (S n).\n\n\
             Theorem mutual_first : forall n, even n -> True\n\
             with mutual_second : forall n, odd n -> True.\n\
             Proof.\n\
             - intros. exact I.\n\
             - intros. exact I.\n\
             Qed.\n",
        )
        .unwrap();
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let project = engine.attach(directory.path()).unwrap();
        let actor = pet::PetActor::new();
        engine
            .reconcile_source(&project, &FileId("A.v".into()))
            .unwrap();
        let declarations = engine
            .declarations(&project, &actor, &FileId("A.v".into()))
            .unwrap();
        assert_eq!(declarations.len(), 2);
        assert!(declarations.iter().all(|declaration| {
            declaration
                .target
                .as_ref()
                .is_some_and(|target| !target.anchor.replaceable)
        }));
    }
}
