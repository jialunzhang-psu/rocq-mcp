//! Per-project attachment ownership for I2.
use crate::repository::{DeclarationIdentity, PendingRecord, ProofRepository, RepositoryError};
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
        let state = root.join("_build/.rocq-engine");
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
}
