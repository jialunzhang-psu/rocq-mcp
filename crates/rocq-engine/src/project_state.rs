//! Per-project attachment ownership for I2.
use crate::repository::{DeclarationIdentity, PendingRecord, ProofRepository, RepositoryError};
use crate::{Error, ErrorKind, layout};
use fs2::FileExt;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

/// Sanitized attachment failure before it crosses the engine boundary.
///
/// Attachment failures distinguish lock contention, corrupt durable recovery,
/// and inability to access the project-owned state directory.
#[derive(Debug)]
pub(crate) enum AttachmentError {
    Contended,
    Recovery,
    ProjectState,
}

/// Durable state is project-owned but not a Dune build artifact: `dune clean`
/// must not erase a solved candidate before publication or recovery.
pub(crate) fn project_state_directory(project: &Path) -> crate::Result<PathBuf> {
    if let Some(workspace) = layout::dune_workspace_root(project) {
        let scope = project.strip_prefix(workspace).map_err(|_| {
            Error::new(
                ErrorKind::InvalidConfiguration,
                "invalid Dune attachment scope",
            )
        })?;
        Ok(workspace.join(".rocq-engine").join(scope))
    } else {
        Ok(project.join("_build/.rocq-engine"))
    }
}

/// Directory that a staging copy must exclude. Non-Dune projects retain the
/// established build-state contract; Dune workspaces keep proofs outside build.
pub(crate) fn excluded_state_tree(project: &Path) -> crate::Result<PathBuf> {
    if let Some(workspace) = layout::dune_workspace_root(project) {
        Ok(workspace.join(".rocq-engine"))
    } else {
        Ok(project.join("_build"))
    }
}

/// Current-view attachment. Its gate never represents a historical generation.
pub(crate) struct ProjectAttachment {
    root: PathBuf,
    _lock: File,
    repository: ProofRepository,
    pending: Mutex<BTreeMap<DeclarationIdentity, PendingRecord>>,
    gate: RwLock<()>,
}
impl ProjectAttachment {
    /// Canonicalizes and exclusively attaches a project, then recovers its
    /// durable proof records before publishing the attachment.
    pub(crate) fn attach(project: &Path) -> Result<Arc<Self>, AttachmentError> {
        let root = fs::canonicalize(project).map_err(|_| AttachmentError::ProjectState)?;
        let state = project_state_directory(&root).map_err(|_| AttachmentError::ProjectState)?;
        if layout::dune_workspace_root(&root).is_some() {
            migrate_legacy_dune_state(&root, &state)?;
        }
        fs::create_dir_all(&state).map_err(|_| AttachmentError::ProjectState)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(state.join("project.lock"))
            .map_err(|_| AttachmentError::ProjectState)?;
        match lock.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(AttachmentError::Contended);
            }
            Err(_) => return Err(AttachmentError::ProjectState),
        }
        let repository = ProofRepository::open(&root).map_err(recovery_error)?;
        let pending = repository.scan().map_err(recovery_error)?;
        Ok(Arc::new(Self {
            root,
            _lock: lock,
            repository,
            pending: Mutex::new(pending),
            gate: RwLock::new(()),
        }))
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, ()> {
        self.gate
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Acquires the exclusive current-view gate for a mutation transaction.
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, ()> {
        self.gate
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Looks up a recovered semantic record without exposing its physical locator.
    pub(crate) fn pending(&self, id: &DeclarationIdentity) -> Option<PendingRecord> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }
    /// Returns all recovered records for one gated current project view.
    pub(crate) fn pending_records(&self) -> BTreeMap<DeclarationIdentity, PendingRecord> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// Reloads one durable binding after an in-memory miss. This repairs a
    /// stale cache without discarding or replacing the append-only record.
    pub(crate) fn recover_pending(
        &self,
        identity: &DeclarationIdentity,
    ) -> Result<Option<PendingRecord>, RepositoryError> {
        let record = self.repository.scan()?.remove(identity);
        if let Some(record) = &record {
            self.pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(identity.clone(), record.clone());
        }
        Ok(record)
    }
    /// Returns the attachment's sole durable repository for a gated transaction.
    pub(crate) fn repository(&self) -> &ProofRepository {
        &self.repository
    }
    /// Persists and publishes one selected solved candidate to the attachment's
    /// identity-indexed recovery view while the caller holds the project write
    /// gate. The repository write is durable before the in-memory binding is
    /// made visible.
    pub(crate) fn persist_candidate(
        &self,
        candidate: crate::repository::SolvedCandidate,
    ) -> Result<PendingRecord, RepositoryError> {
        let identity = candidate.declaration.identity.clone();
        let record = self.repository.persist(candidate)?;
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(identity, record.clone());
        Ok(record)
    }
    pub(crate) fn remove_pending(&self, identity: &DeclarationIdentity) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.remove(identity);
    }
    /// Durably replaces a solved record with its deterministic rejection and
    /// updates the attachment's recovery view under the project write gate.
    pub(crate) fn reject_candidate(
        &self,
        identity: &DeclarationIdentity,
        rejection: crate::repository::CandidateRejection,
    ) -> Result<PendingRecord, RepositoryError> {
        let record = self.repository.reject(identity, rejection)?;
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(identity.clone(), record.clone());
        Ok(record)
    }
    /// Promotes the durable solved record before any project source mutation.
    pub(crate) fn promote_candidate(
        &self,
        identity: &DeclarationIdentity,
        proof: crate::repository::ClosedProof,
    ) -> Result<PendingRecord, RepositoryError> {
        let record = self.repository.promote(identity, proof)?;
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(identity.clone(), record.clone());
        Ok(record)
    }
}

/// Migrates the old source-local state only while holding its lifetime lock.
/// A live old server prevents attachment; conflicting durable stores fail
/// closed rather than silently choosing one set of proof records.
fn migrate_legacy_dune_state(
    project: &Path,
    destination: &Path,
) -> std::result::Result<(), AttachmentError> {
    let legacy = project.join("_build/.rocq-engine");
    if !legacy.exists() || legacy == destination {
        return Ok(());
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(legacy.join("project.lock"))
        .map_err(|_| AttachmentError::ProjectState)?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            return Err(AttachmentError::Contended);
        }
        Err(_) => return Err(AttachmentError::ProjectState),
    }
    if destination.exists() {
        return Err(AttachmentError::Recovery);
    }
    fs::create_dir_all(destination.parent().ok_or(AttachmentError::ProjectState)?)
        .map_err(|_| AttachmentError::ProjectState)?;
    fs::rename(&legacy, destination).map_err(|_| AttachmentError::ProjectState)?;
    Ok(())
}
impl Drop for ProjectAttachment {
    fn drop(&mut self) {
        let _ = self._lock.unlock();
    }
}
/// Engine-local canonical registry; only the map lookup is global, never PET/build work.
enum Slot {
    Ready(Arc<ProjectAttachment>),
    Creating,
}
pub(crate) struct ProjectRegistry {
    values: Mutex<BTreeMap<PathBuf, Slot>>,
    changed: Condvar,
}
impl ProjectRegistry {
    pub(crate) fn new() -> Self {
        Self {
            values: Mutex::new(BTreeMap::new()),
            changed: Condvar::new(),
        }
    }
    /// Returns the canonical project's single-flight attachment.
    ///
    /// The registry mutex protects slots only. Filesystem work and durable
    /// recovery run after the creating slot is published and the mutex dropped.
    pub(crate) fn attach(&self, path: &Path) -> Result<Arc<ProjectAttachment>, AttachmentError> {
        let root = fs::canonicalize(path).map_err(|_| AttachmentError::ProjectState)?;
        let mut values = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            match values.get(&root) {
                Some(Slot::Ready(value)) => return Ok(value.clone()),
                Some(Slot::Creating) => {
                    values = self
                        .changed
                        .wait(values)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                }
                None => {
                    values.insert(root.clone(), Slot::Creating);
                    break;
                }
            }
        }
        drop(values);
        let created = ProjectAttachment::attach(&root);
        let mut values = self
            .values
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match created {
            Ok(value) => {
                values.insert(root, Slot::Ready(value.clone()));
                self.changed.notify_all();
                Ok(value)
            }
            Err(error) => {
                values.remove(&root);
                self.changed.notify_all();
                Err(error)
            }
        }
    }
}

/// Preserves contention/unavailability semantics while retaining fail-closed
/// durable-shape errors for the engine's public error mapper.
fn recovery_error(error: RepositoryError) -> AttachmentError {
    match error {
        RepositoryError::Storage => AttachmentError::ProjectState,
        _ => AttachmentError::Recovery,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachment_owns_lock_pending_and_gates() {
        let t = tempfile::tempdir().unwrap();
        let a = ProjectAttachment::attach(t.path()).unwrap();
        assert!(ProjectAttachment::attach(t.path()).is_err());
        let _r = a.read();
        drop(_r);
        let _w = a.write();
        drop(_w);
        assert!(
            a.pending(&DeclarationIdentity {
                library: crate::repository::LogicalLibrary(vec!["L".into()]),
                modules: vec![],
                constant: "t".into()
            })
            .is_none()
        );
        let _ = a.repository();
    }
    #[test]
    fn registry_reuses_canonical_attachment() {
        let t = tempfile::tempdir().unwrap();
        let r = ProjectRegistry::new();
        assert!(Arc::ptr_eq(
            &r.attach(t.path()).unwrap(),
            &r.attach(t.path()).unwrap()
        ));
    }

    #[test]
    fn dune_state_is_durable_outside_build_and_migrates_under_old_lock() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("dune-project"), "(lang dune 3.21)\n").unwrap();
        fs::create_dir(project.path().join("Library")).unwrap();
        let library = project.path().join("Library");
        let legacy = library.join("_build/.rocq-engine");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("project.lock"), "").unwrap();
        let attached = ProjectAttachment::attach(&library).unwrap();
        assert!(
            project
                .path()
                .join(".rocq-engine/Library/project.lock")
                .is_file()
        );
        assert!(!legacy.exists());
        drop(attached);
    }

    #[test]
    fn live_legacy_dune_attachment_prevents_parallel_migration() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("dune-project"), "(lang dune 3.21)\n").unwrap();
        let legacy = project.path().join("_build/.rocq-engine");
        fs::create_dir_all(&legacy).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(legacy.join("project.lock"))
            .unwrap();
        lock.try_lock_exclusive().unwrap();
        assert!(matches!(
            ProjectAttachment::attach(project.path()),
            Err(AttachmentError::Contended)
        ));
        assert!(legacy.exists());
    }

    #[test]
    fn legacy_dune_state_bytes_survive_migration() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("dune-project"), "(lang dune 3.21)\n").unwrap();
        let legacy = project.path().join("_build/.rocq-engine");
        fs::create_dir_all(legacy.join("proofs")).unwrap();
        fs::write(legacy.join("proofs/bad-record"), b"recover-me").unwrap();
        let _attached = ProjectAttachment::attach(project.path()).unwrap();
        assert_eq!(
            fs::read(project.path().join(".rocq-engine/proofs/bad-record")).unwrap(),
            b"recover-me"
        );
        assert!(!legacy.exists());
    }
}
