//! Stateless sequencing facade over the Dune, PET, and writeback wrappers.
//!
//! Active projects and checkpoint topology belong to `rocq-mcp`; `Engine`
//! retains only operator configuration and never stores declarations, attempts,
//! traces, or PET state IDs.

use crate::types::{PetRange, PetWorkspace, SourceAnchor};
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

    fn library(&self, source: &Path) -> Result<LogicalLibrary> {
        self.read_layout().library(source).cloned()
    }

    fn source_for_constant(&self, constant: &str) -> Option<PathBuf> {
        self.read_layout().source_for_constant(constant)
    }

    fn files(&self) -> Vec<PathBuf> {
        self.read_layout().files()
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
        Ok(DuneProject {
            root,
            layout: Arc::new(RwLock::new(layout)),
        })
    }

    /// Re-query Dune and atomically replace the cached typed view when its
    /// selected files, libraries, targets, or PET load paths changed.
    ///
    /// Returns `true` exactly when the caller must begin a new PET epoch and
    /// invalidate all exported state IDs for this project. A workspace-root
    /// identity change fails closed and requires an explicit `start`.
    pub fn refresh_project(&self, project: &DuneProject) -> Result<bool> {
        let (root, layout) = dune::Layout::load(&project.root, self.config.command_timeout)?;
        if root != project.root {
            return Err(Error::new(
                ErrorKind::InvalidConfiguration,
                "Dune workspace identity changed; call start again",
            ));
        }
        Ok(project.replace_layout(layout))
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
                "declaration shares one PET source command with another declaration",
            ));
        }
        self.open_target(project, actor, target)
            .map(OpenResult::Open)
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
        let insertion = actor
            .insertion_point(&workspace, &source, modules)
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
                "PET insertion range is invalid",
            ));
        }
        let header = format!("{} {} : {}", kind.keyword(), leaf, statement);
        let base = actor
            .state_at(&workspace, &source, insertion)
            .map_err(declaration_pet_error)?;
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
                    &target.anchor.source,
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
                        "target identity changed in the current Dune/PET context",
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
    ) -> Result<ProofStep> {
        self.validate_target(project, target)?;
        validate_fragment(fragment)?;
        let execution = actor.run(state, fragment).map_err(step_pet_error)?;
        if !execution.goals.proof_mode || !execution.goals.given_up.is_empty() {
            actor
                .release_states(&[execution.state])
                .map_err(step_pet_error)?;
            return Err(Error::new(
                ErrorKind::ProofStepFailed,
                "PET rejected a proof-closing, global, or goal-giving-up fragment",
            ));
        }
        Ok(ProofStep {
            state: execution.state,
            view: proof_state(target.info.clone(), &execution.goals),
            finished: execution.proof_finished,
        })
    }

    pub fn goals(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
        state: pet::PetStateId,
    ) -> Result<ProofState> {
        self.validate_target(project, target)?;
        let goals = actor.goals(state).map_err(step_pet_error)?;
        if !goals.proof_mode {
            return Err(Error::new(
                ErrorKind::NotFound,
                "PET proof state is no longer open",
            ));
        }
        Ok(proof_state(target.info.clone(), &goals))
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
        let context = actor
            .state_at(
                &workspace,
                &target.anchor.source,
                target.anchor.declaration.end,
            )
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

    /// Build, refresh, reopen, and trust-audit a PET-finished target.
    /// The MCP caller must hold the project publication barrier and invalidate
    /// all checkpoint state IDs before calling.
    pub fn validate_published(
        &self,
        project: &DuneProject,
        actor: &pet::PetActor,
        target: &DeclarationTarget,
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
            |project, actor| {
                // The build runs outside Rust's source CAS.  Check again
                // after it returns so an editor racing the build cannot make
                // a different declaration appear completed.
                self.validate_target(project, target)?;
                let resolved = self.resolve(project, actor, &target.info.identity)?;
                if !resolved.proof_finished {
                    return Err(Error::new(
                        ErrorKind::InvalidDeclaration,
                        "built declaration is not a completed PET proof",
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
            .map_err(step_pet_error)?;
        if closed.goals.proof_mode || !closed.proof_finished {
            actor
                .release_states(&[closed.state])
                .map_err(step_pet_error)?;
            return Err(Error::new(
                ErrorKind::ProofStepFailed,
                "PET did not close the proof",
            ));
        }
        actor
            .release_states(&[closed.state])
            .map_err(step_pet_error)
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
        self.validate_target(project, target)?;
        let publication = writeback::prepare(project, target, fragments)?;
        begin_epoch();
        writeback::publish(
            project,
            actor,
            publication,
            self.config.command_timeout,
            |project, actor| {
                let resolved = self.resolve(project, actor, &target.info.identity)?;
                if !resolved.proof_finished {
                    return Err(Error::new(
                        ErrorKind::InvalidDeclaration,
                        "written declaration is not a completed PET proof",
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
        let opened = actor
            .state_at(&workspace, &target.anchor.source, target.anchor.header.end)
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
        let rows = actor
            .document_declarations(&workspace, &source)
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
                    "PET returned invalid declaration metadata",
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
                            "PET declaration path '{}' disagrees with Dune compilation unit '{}'",
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
                        "PET declaration identity is ambiguous",
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
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "declaration was not returned by PET"))
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
            .state_at(&workspace, &target.anchor.source, bytes.len())
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
            .query(
                state,
                &PetQuery::Assumptions(target.info.identity.qualified_name()),
            )
            .map_err(query_pet_error)?;
        let assumptions = parse_assumptions(&report)?;
        for printed in assumptions {
            let located = actor
                .query(state, &PetQuery::Locate(printed.clone()))
                .map_err(query_pet_error)?;
            let canonical = locate_constant(&located).ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidConfiguration,
                    "PET Locate returned no unique constant",
                )
            })?;
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
                        format!("PET could not classify axiom dependency: {canonical}"),
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
            "PET did not open a safe proof state",
        ));
    }
    Ok(())
}

fn proof_state(info: DeclarationInfo, goals: &pet::PetGoals) -> ProofState {
    ProofState {
        theorem: info,
        lifecycle: ProofLifecycle::Open,
        focused_goals: goals.focused.len(),
        unfocused_goals: goals.unfocused.len(),
        shelved_goals: goals.shelved.len(),
        given_up_goals: goals.given_up.len(),
        goals: render_goals(goals),
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
        focused_goals: 0,
        unfocused_goals: 0,
        shelved_goals: 0,
        given_up_goals: 0,
        goals: String::new(),
    }
}

fn render_goals(goals: &pet::PetGoals) -> String {
    let mut rendered = Vec::new();
    for (index, goal) in goals.focused.iter().enumerate() {
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
        rendered.push(lines.join("\n"));
    }
    rendered.join("\n\n")
}

fn parse_assumptions(report: &str) -> Result<Vec<String>> {
    if report.contains("Closed under the global context") {
        return Ok(Vec::new());
    }
    let names = report
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (name, _) = line.split_once(" : ")?;
            (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_owned())
        })
        .collect::<Vec<_>>();
    if names.is_empty() {
        Err(Error::new(
            ErrorKind::InvalidConfiguration,
            "PET assumption result is unrecognized",
        ))
    } else {
        Ok(names)
    }
}

fn locate_constant(report: &str) -> Option<String> {
    report.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Constant ")?
            .split_whitespace()
            .next()
            .map(str::to_owned)
    })
}

fn declaration_pet_error(error: pet::PetError) -> Error {
    let kind = match &error {
        error if error.lost() => ErrorKind::PetLost,
        pet::PetError::Environment(_) => ErrorKind::InvalidConfiguration,
        pet::PetError::Invalid(_) | pet::PetError::Remote { .. } => ErrorKind::InvalidDeclaration,
        pet::PetError::Protocol(_) | pet::PetError::OutputOverflow => ErrorKind::PetLost,
        pet::PetError::ProcessLost(_) => ErrorKind::PetLost,
    };
    Error::new(kind, error.to_string())
}

fn step_pet_error(error: pet::PetError) -> Error {
    let kind = match &error {
        error if error.lost() => ErrorKind::PetLost,
        pet::PetError::Environment(_) => ErrorKind::InvalidConfiguration,
        pet::PetError::Invalid(_) => ErrorKind::InvalidRequest,
        pet::PetError::Remote { .. } => ErrorKind::ProofStepFailed,
        pet::PetError::Protocol(_) | pet::PetError::OutputOverflow => ErrorKind::PetLost,
        pet::PetError::ProcessLost(_) => ErrorKind::PetLost,
    };
    Error::new(kind, error.to_string())
}

fn query_pet_error(error: pet::PetError) -> Error {
    let kind = match &error {
        error if error.lost() => ErrorKind::PetLost,
        pet::PetError::Invalid(_) => ErrorKind::InvalidRequest,
        pet::PetError::Environment(_)
        | pet::PetError::Remote { .. }
        | pet::PetError::Protocol(_)
        | pet::PetError::OutputOverflow
        | pet::PetError::ProcessLost(_) => ErrorKind::InvalidConfiguration,
    };
    Error::new(kind, error.to_string())
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
