//! Durable, self-validating proof records.  This module owns no source edits.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};
use uuid::Uuid;

/// Canonical Rocq compilation-unit name, never a filesystem path.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalLibrary(pub Vec<String>);
/// Nested lexical scopes. Sections preserve placement context but not constant paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LexicalScope {
    Module(String),
    Section(String),
}
/// Structured declaration address; it prevents library/module boundary guessing.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarationIdentity {
    pub library: LogicalLibrary,
    pub modules: Vec<String>,
    pub constant: String,
}
/// Closing policy is determined solely by declaration kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeclarationKind {
    Theorem,
    Lemma,
    Fact,
    Remark,
    Corollary,
    Proposition,
    Definition,
}
impl DeclarationKind {
    pub fn terminator(self) -> &'static str {
        if self == Self::Definition {
            "Defined."
        } else {
            "Qed."
        }
    }
}
/// Admission request with resolved logical placement, not a caller filesystem path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewDeclaration {
    pub kind: DeclarationKind,
    pub identity: DeclarationIdentity,
    pub context: Vec<LexicalScope>,
    pub statement: String,
}
/// Semantic source anchor. Byte ranges remain private transient parser output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAnchor {
    pub source_digest: [u8; 32],
    pub normalized_statement: String,
    pub context: Vec<LexicalScope>,
    pub old_body_digest: [u8; 32],
}
/// Forest root authority, deliberately independent of a historical project generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenDeclaration {
    pub kind: DeclarationKind,
    pub identity: DeclarationIdentity,
    pub anchor: SourceAnchor,
}
/// One scanner-accepted proof sentence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CanonicalTactic(pub String);
/// Content-addressed fail-closed trust authorization frozen at solve time.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustBaseline {
    identity: [u8; 32],
    pub explicit_axioms: Vec<(String, String)>,
    pub external_artifacts: Vec<(LogicalLibrary, [u8; 32])>,
    pub forbidden_locals: Vec<String>,
    pub toolchain: [u8; 32],
}
impl TrustBaseline {
    pub(crate) fn new(
        mut explicit_axioms: Vec<(String, String)>,
        mut external_artifacts: Vec<(LogicalLibrary, [u8; 32])>,
        mut forbidden_locals: Vec<String>,
        toolchain: [u8; 32],
    ) -> Result<Self, RepositoryError> {
        explicit_axioms.sort();
        explicit_axioms.dedup();
        external_artifacts.sort();
        external_artifacts.dedup();
        forbidden_locals.sort();
        forbidden_locals.dedup();
        let mut value = Self {
            identity: [0; 32],
            explicit_axioms,
            external_artifacts,
            forbidden_locals,
            toolchain,
        };
        validate_baseline(&value)?;
        value.identity = baseline_identity(&value)?;
        Ok(value)
    }
    pub fn identity(&self) -> [u8; 32] {
        self.identity
    }
}
/// The only legal declaration terminators, derived from DeclarationKind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProofTerminator {
    Qed,
    Defined,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TrustAuditResult {
    ClosedUnderFrozenBaseline,
}
/// Ordered replacement with a safe relative target and constructor-derived digest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileReplacement {
    relative: PathBuf,
    pub old_digest: Option<[u8; 32]>,
    #[serde(with = "bounded_base64")]
    old_contents: Vec<u8>,
    #[serde(with = "bounded_base64")]
    pub contents: Vec<u8>,
    new_digest: [u8; 32],
}
impl FileReplacement {
    fn from_original(
        relative: PathBuf,
        old_contents: Option<Vec<u8>>,
        contents: Vec<u8>,
    ) -> Result<Self, RepositoryError> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|x| !matches!(x, std::path::Component::Normal(_)))
        {
            return Err(RepositoryError::Invalid);
        }
        if contents.len() > MAX_REPLACEMENT_BYTES {
            return Err(RepositoryError::Oversize);
        }
        let new_digest = Sha256::digest(&contents).into();
        Ok(Self {
            relative,
            old_digest: old_contents
                .as_ref()
                .map(|bytes| Sha256::digest(bytes).into()),
            old_contents: old_contents.unwrap_or_default(),
            contents,
            new_digest,
        })
    }
    /// Constructs a crash-recoverable replacement containing the exact prior
    /// bytes. `None` represents a file that did not exist.
    pub(crate) fn with_original(
        relative: PathBuf,
        old_contents: Option<Vec<u8>>,
        contents: Vec<u8>,
    ) -> Result<Self, RepositoryError> {
        if old_contents
            .as_ref()
            .is_some_and(|bytes| bytes.len() > MAX_REPLACEMENT_BYTES)
        {
            return Err(RepositoryError::Oversize);
        }
        Self::from_original(relative, old_contents, contents)
    }
    pub fn relative(&self) -> &Path {
        &self.relative
    }
    pub fn old_digest(&self) -> Option<[u8; 32]> {
        self.old_digest
    }
    pub fn old_contents(&self) -> Option<&[u8]> {
        self.old_digest.map(|_| self.old_contents.as_slice())
    }
    pub fn new_digest(&self) -> [u8; 32] {
        self.new_digest
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolvedCandidate {
    pub declaration: OpenDeclaration,
    pub commands: Vec<CanonicalTactic>,
    pub baseline: TrustBaseline,
    content_identity: [u8; 32],
}
impl SolvedCandidate {
    pub(crate) fn new(
        declaration: OpenDeclaration,
        commands: Vec<CanonicalTactic>,
        baseline: TrustBaseline,
    ) -> Result<Self, RepositoryError> {
        let mut value = Self {
            declaration,
            commands,
            baseline,
            content_identity: [0; 32],
        };
        validate_candidate_shape(&value)?;
        value.content_identity = candidate_identity(&value)?;
        Ok(value)
    }
    pub fn content_identity(&self) -> [u8; 32] {
        self.content_identity
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedProof {
    pub candidate: SolvedCandidate,
    terminator: ProofTerminator,
    pub audit: TrustAuditResult,
    pub replacements: Vec<FileReplacement>,
}
impl ClosedProof {
    pub(crate) fn new(
        candidate: SolvedCandidate,
        audit: TrustAuditResult,
        replacements: Vec<FileReplacement>,
    ) -> Result<Self, RepositoryError> {
        let terminator = if candidate.declaration.kind == DeclarationKind::Definition {
            ProofTerminator::Defined
        } else {
            ProofTerminator::Qed
        };
        let value = Self {
            candidate,
            terminator,
            audit,
            replacements,
        };
        validate_closed(&value)?;
        Ok(value)
    }
    pub fn terminator(&self) -> ProofTerminator {
        self.terminator
    }
}
/// Deterministic close stage that rejected the selected candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RejectionPhase {
    NativeValidation,
    TrustAudit,
}
/// Sanitized diagnostic for a candidate rejection. It has no path or process identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    code: String,
    message: String,
}
impl Diagnostic {
    pub(crate) fn new(
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, RepositoryError> {
        let code = code.into();
        let message = message.into();
        if code.is_empty()
            || code.len() > 64
            || message.is_empty()
            || message.len() > 4096
            || !code
                .chars()
                .all(|value| value == '_' || value.is_ascii_alphanumeric())
            || message.chars().any(char::is_control)
        {
            return Err(RepositoryError::Invalid);
        }
        Ok(Self { code, message })
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}
/// Immutable rejection remains bound to its candidate until project content changes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRejection {
    candidate: SolvedCandidate,
    phase: RejectionPhase,
    diagnostics: Vec<Diagnostic>,
}
impl CandidateRejection {
    pub(crate) fn new(
        candidate: SolvedCandidate,
        phase: RejectionPhase,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<Self, RepositoryError> {
        validate_candidate(&candidate)?;
        if diagnostics.is_empty() || diagnostics.len() > MAX_DIAGNOSTICS {
            return Err(RepositoryError::Invalid);
        }
        Ok(Self {
            candidate,
            phase,
            diagnostics,
        })
    }
    pub fn candidate(&self) -> &SolvedCandidate {
        &self.candidate
    }
    pub fn phase(&self) -> RejectionPhase {
        self.phase
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProofPhase {
    Solved(SolvedCandidate),
    Closed(ClosedProof),
    Rejected(CandidateRejection),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    id: Uuid,
    phase: ProofPhase,
    checksum: [u8; 32],
}
/// Semantic recovery view. Physical UUIDs and filesystem locations are private.
#[derive(Clone, Debug)]
pub struct PendingRecord {
    pub phase: ProofPhase,
}
#[derive(Clone)]
struct StoredRecord {
    id: Uuid,
    path: PathBuf,
    phase: ProofPhase,
}
/// Repository errors are intentionally diagnostic-free at the transport boundary.
#[derive(Debug)]
pub(crate) enum RepositoryError {
    Storage,
    Corrupt,
    Duplicate,
    Invalid,
    IllegalTransition,
    Oversize,
}
/// Bounded durable payloads: source snapshots already cap one replay input at 16 MiB.
const MAX_REPLACEMENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPLACEMENTS: usize = 256;
const MAX_COMMANDS: usize = 64;
const MAX_AUTHORIZATIONS: usize = 4096;
const MAX_DIAGNOSTICS: usize = 128;

/// Owns stable UUID records beneath the project build state.
pub(crate) struct ProofRepository {
    directory: PathBuf,
    /// Design note: this index linearizes calls for one repository instance.
    /// I2's project lifetime lock is the separate cross-instance boundary.
    index: Mutex<BTreeMap<DeclarationIdentity, StoredRecord>>,
}
impl ProofRepository {
    /// Opens the durable store, removes incomplete temporary writes, fsyncs a
    /// newly-created hierarchy, then builds a fail-closed in-memory index.
    pub(crate) fn open(project: &Path) -> Result<Self, RepositoryError> {
        let directory = crate::project_state::project_state_directory(project)
            .map_err(|_| RepositoryError::Storage)?
            .join("proofs");
        let existed = directory.exists();
        fs::create_dir_all(&directory).map_err(io_error)?;
        if !existed {
            sync_hierarchy(&directory)?;
        }
        clean_temps(&directory)?;
        let index = load_index(&directory)?;
        Ok(Self {
            directory,
            index: Mutex::new(index),
        })
    }
    /// Persists one previously-unbound solved candidate. Duplicate identities
    /// are rejected before any write, preserving at-most-one candidate.
    pub(crate) fn persist(
        &self,
        candidate: SolvedCandidate,
    ) -> Result<PendingRecord, RepositoryError> {
        validate_candidate(&candidate)?;
        let identity = candidate.declaration.identity.clone();
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if index.contains_key(&identity) {
            return Err(RepositoryError::Duplicate);
        }
        let stored = self.write(ProofPhase::Solved(candidate), None)?;
        index.insert(identity, stored.clone());
        Ok(semantic(stored))
    }
    /// Promotes exactly a stored solved candidate. A caller cannot substitute
    /// a different trace, baseline or replacement plan during promotion.
    pub(crate) fn promote(
        &self,
        identity: &DeclarationIdentity,
        proof: ClosedProof,
    ) -> Result<PendingRecord, RepositoryError> {
        validate_closed(&proof)?;
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = index
            .get(identity)
            .cloned()
            .ok_or(RepositoryError::Invalid)?;
        let ProofPhase::Solved(candidate) = &stored.phase else {
            return Err(RepositoryError::IllegalTransition);
        };
        if candidate != &proof.candidate {
            return Err(RepositoryError::Invalid);
        }
        let replacement = self.write(ProofPhase::Closed(proof), Some(stored.id))?;
        index.insert(identity.clone(), replacement.clone());
        Ok(semantic(replacement))
    }
    /// Records an immutable deterministic rejection of the stored solved
    /// candidate. Rejected records cannot be replaced until project content changes in I2.
    pub(crate) fn reject(
        &self,
        identity: &DeclarationIdentity,
        rejection: CandidateRejection,
    ) -> Result<PendingRecord, RepositoryError> {
        validate_candidate(&rejection.candidate)?;
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = index
            .get(identity)
            .cloned()
            .ok_or(RepositoryError::Invalid)?;
        let ProofPhase::Solved(candidate) = &stored.phase else {
            return Err(RepositoryError::IllegalTransition);
        };
        if candidate != &rejection.candidate {
            return Err(RepositoryError::Invalid);
        }
        let replacement = self.write(ProofPhase::Rejected(rejection), Some(stored.id))?;
        index.insert(identity.clone(), replacement.clone());
        Ok(semantic(replacement))
    }
    /// Returns semantic pending views only; physical UUIDs and paths stay private.
    pub(crate) fn scan(
        &self,
    ) -> Result<BTreeMap<DeclarationIdentity, PendingRecord>, RepositoryError> {
        let index = self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(index
            .iter()
            .map(|(identity, stored)| (identity.clone(), semantic(stored.clone())))
            .collect())
    }
    /// Acknowledges only a fully closed proof. Solved and rejected candidates
    /// are recovery authorities and must never be deleted by this API.
    pub(crate) fn acknowledge_closed(
        &self,
        identity: &DeclarationIdentity,
    ) -> Result<(), RepositoryError> {
        let mut index = self
            .index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stored = index
            .get(identity)
            .cloned()
            .ok_or(RepositoryError::Invalid)?;
        if !matches!(stored.phase, ProofPhase::Closed(_)) {
            return Err(RepositoryError::IllegalTransition);
        }
        fs::remove_file(&stored.path).map_err(io_error)?;
        File::open(&self.directory)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)?;
        index.remove(identity);
        Ok(())
    }
    fn write(
        &self,
        phase: ProofPhase,
        existing: Option<Uuid>,
    ) -> Result<StoredRecord, RepositoryError> {
        let id = existing.unwrap_or_else(Uuid::now_v7);
        let record = Record {
            version: 1,
            id,
            checksum: checksum(id, &phase)?,
            phase,
        };
        let bytes = serde_json::to_vec(&record).map_err(|_| RepositoryError::Invalid)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(RepositoryError::Oversize);
        }
        let path = self.path(id);
        let temporary = self.directory.join(format!(".{id}.tmp"));
        let result = (|| -> Result<(), RepositoryError> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(io_error)?;
            file.write_all(&bytes).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            fs::rename(&temporary, &path).map_err(io_error)?;
            File::open(&self.directory)
                .map_err(io_error)?
                .sync_all()
                .map_err(io_error)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        Ok(StoredRecord {
            id,
            phase: record.phase,
            path,
        })
    }
    fn path(&self, id: Uuid) -> PathBuf {
        self.directory.join(format!("{id}.json"))
    }
}

fn io_error(_: std::io::Error) -> RepositoryError {
    RepositoryError::Storage
}
fn sync_hierarchy(directory: &Path) -> Result<(), RepositoryError> {
    for path in [
        directory,
        directory.parent().ok_or(RepositoryError::Storage)?,
        directory
            .parent()
            .and_then(Path::parent)
            .ok_or(RepositoryError::Storage)?,
        directory
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(RepositoryError::Storage)?,
    ] {
        File::open(path)
            .map_err(io_error)?
            .sync_all()
            .map_err(io_error)?;
    }
    Ok(())
}
fn clean_temps(directory: &Path) -> Result<(), RepositoryError> {
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        if path
            .file_name()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.starts_with('.') && x.ends_with(".tmp"))
        {
            fs::remove_file(path).map_err(io_error)?;
        }
    }
    File::open(directory)
        .map_err(io_error)?
        .sync_all()
        .map_err(io_error)
}
fn read_record(path: &Path) -> Result<Record, RepositoryError> {
    let metadata = fs::metadata(path).map_err(io_error)?;
    if metadata.len() > MAX_RECORD_BYTES {
        return Err(RepositoryError::Oversize);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)
        .map_err(io_error)?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| RepositoryError::Corrupt)?;
    let filename = path
        .file_stem()
        .and_then(|x| x.to_str())
        .and_then(|x| Uuid::parse_str(x).ok())
        .ok_or(RepositoryError::Corrupt)?;
    if filename != record.id
        || record.version != 1
        || checksum(record.id, &record.phase)? != record.checksum
    {
        return Err(RepositoryError::Corrupt);
    };
    validate_phase(&record.phase)?;
    Ok(record)
}
fn load_index(
    directory: &Path,
) -> Result<BTreeMap<DeclarationIdentity, StoredRecord>, RepositoryError> {
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let record = read_record(&path)?;
        let identity = identity(&record.phase).clone();
        if out
            .insert(
                identity,
                StoredRecord {
                    id: record.id,
                    path,
                    phase: record.phase,
                },
            )
            .is_some()
        {
            return Err(RepositoryError::Duplicate);
        }
    }
    Ok(out)
}

fn validate_phase(phase: &ProofPhase) -> Result<(), RepositoryError> {
    match phase {
        ProofPhase::Solved(x) => validate_candidate(x),
        ProofPhase::Closed(x) => validate_closed(x),
        ProofPhase::Rejected(x) => {
            validate_candidate(x.candidate())?;
            if x.diagnostics.len() > MAX_DIAGNOSTICS
                || x.diagnostics()
                    .iter()
                    .any(|item| Diagnostic::new(item.code(), item.message()).is_err())
            {
                return Err(RepositoryError::Invalid);
            }
            Ok(())
        }
    }
}
fn checked_total(values: impl Iterator<Item = usize>, limit: usize) -> Result<(), RepositoryError> {
    let mut total = 0usize;
    for value in values {
        total = total.checked_add(value).ok_or(RepositoryError::Oversize)?;
        if total > limit {
            return Err(RepositoryError::Oversize);
        }
    }
    Ok(())
}
fn validate_baseline(value: &TrustBaseline) -> Result<(), RepositoryError> {
    if value.explicit_axioms.len() > MAX_AUTHORIZATIONS
        || value.external_artifacts.len() > MAX_AUTHORIZATIONS
        || value.forbidden_locals.len() > MAX_AUTHORIZATIONS
    {
        return Err(RepositoryError::Oversize);
    }
    if !value.explicit_axioms.windows(2).all(|x| x[0] < x[1])
        || !value.external_artifacts.windows(2).all(|x| x[0] < x[1])
        || !value.forbidden_locals.windows(2).all(|x| x[0] < x[1])
    {
        return Err(RepositoryError::Invalid);
    }
    for (name, ty) in &value.explicit_axioms {
        if name.len() > 64 * 1024 || ty.len() > 64 * 1024 {
            return Err(RepositoryError::Oversize);
        }
        if !valid_qualified_identifier(name) || ty.is_empty() || ty.chars().any(char::is_control) {
            return Err(RepositoryError::Invalid);
        }
    }
    for (library, _) in &value.external_artifacts {
        if library.0.is_empty() {
            return Err(RepositoryError::Invalid);
        }
        for component in &library.0 {
            if component.len() > 64 * 1024 {
                return Err(RepositoryError::Oversize);
            }
            if !valid_identifier(component) {
                return Err(RepositoryError::Invalid);
            }
        }
    }
    for name in &value.forbidden_locals {
        if name.len() > 64 * 1024 {
            return Err(RepositoryError::Oversize);
        }
        if !valid_identifier(name) {
            return Err(RepositoryError::Invalid);
        }
    }
    checked_total(
        value
            .explicit_axioms
            .iter()
            .map(|x| x.0.len().saturating_add(x.1.len()))
            .chain(value.forbidden_locals.iter().map(String::len)),
        1024 * 1024,
    )
}
fn validate_candidate_shape(candidate: &SolvedCandidate) -> Result<(), RepositoryError> {
    if candidate.commands.len() > MAX_COMMANDS {
        return Err(RepositoryError::Oversize);
    }
    for command in &candidate.commands {
        if command.0.len() > 64 * 1024 {
            return Err(RepositoryError::Oversize);
        }
    }
    checked_total(candidate.commands.iter().map(|x| x.0.len()), 1024 * 1024)?;
    if !valid_identity(&candidate.declaration.identity)
        || candidate.declaration.anchor.normalized_statement.is_empty()
        || candidate.declaration.anchor.normalized_statement.len() > 64 * 1024
        || candidate.commands.iter().any(|x| x.0.is_empty())
    {
        return Err(RepositoryError::Invalid);
    }
    let modules: Vec<_> = candidate
        .declaration
        .anchor
        .context
        .iter()
        .filter_map(|x| {
            if let LexicalScope::Module(y) = x {
                Some(y)
            } else {
                None
            }
        })
        .collect();
    if modules
        != candidate
            .declaration
            .identity
            .modules
            .iter()
            .collect::<Vec<_>>()
    {
        return Err(RepositoryError::Invalid);
    }
    validate_baseline(&candidate.baseline)
}
fn validate_candidate(candidate: &SolvedCandidate) -> Result<(), RepositoryError> {
    validate_candidate_shape(candidate)?;
    if candidate.content_identity() != candidate_identity(candidate)?
        || candidate.baseline.identity() != baseline_identity(&candidate.baseline)?
    {
        return Err(RepositoryError::Invalid);
    }
    Ok(())
}

fn validate_closed(proof: &ClosedProof) -> Result<(), RepositoryError> {
    validate_candidate(&proof.candidate)?;
    let expected = if proof.candidate.declaration.kind == DeclarationKind::Definition {
        ProofTerminator::Defined
    } else {
        ProofTerminator::Qed
    };
    if proof.terminator() != expected {
        return Err(RepositoryError::Invalid);
    }
    if proof.replacements.len() > MAX_REPLACEMENTS {
        return Err(RepositoryError::Oversize);
    }
    if proof
        .replacements
        .iter()
        .try_fold(0usize, |total, item| {
            total
                .checked_add(item.contents.len())?
                .checked_add(item.old_contents.len())
        })
        .is_none_or(|total| total > MAX_RECORD_BYTES as usize)
    {
        return Err(RepositoryError::Oversize);
    }
    for replacement in &proof.replacements {
        if replacement.relative().as_os_str().is_empty()
            || replacement
                .relative()
                .components()
                .any(|x| !matches!(x, std::path::Component::Normal(_)))
            || replacement.new_digest() != <[u8; 32]>::from(Sha256::digest(&replacement.contents))
            || (replacement.old_digest.is_some()
                && replacement.old_digest
                    != Some(<[u8; 32]>::from(Sha256::digest(&replacement.old_contents))))
        {
            return Err(RepositoryError::Invalid);
        }
    }
    Ok(())
}
fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|x| x.is_control() || x.is_whitespace() || matches!(x, '.' | '/' | '\\'))
}
fn valid_qualified_identifier(value: &str) -> bool {
    value.split('.').all(valid_identifier)
}
fn valid_identity(identity: &DeclarationIdentity) -> bool {
    !identity.library.0.is_empty()
        && valid_identifier(&identity.constant)
        && identity
            .library
            .0
            .iter()
            .chain(identity.modules.iter())
            .all(|x| valid_identifier(x))
}

fn baseline_identity(baseline: &TrustBaseline) -> Result<[u8; 32], RepositoryError> {
    let mut x = baseline.clone();
    x.identity = [0; 32];
    Ok(Sha256::digest(serde_json::to_vec(&x).map_err(|_| RepositoryError::Invalid)?).into())
}
fn candidate_identity(candidate: &SolvedCandidate) -> Result<[u8; 32], RepositoryError> {
    let mut x = candidate.clone();
    x.content_identity = [0; 32];
    Ok(Sha256::digest(serde_json::to_vec(&x).map_err(|_| RepositoryError::Invalid)?).into())
}

fn semantic(record: StoredRecord) -> PendingRecord {
    PendingRecord {
        phase: record.phase,
    }
}
fn identity(phase: &ProofPhase) -> &DeclarationIdentity {
    match phase {
        ProofPhase::Solved(x) => &x.declaration.identity,
        ProofPhase::Closed(x) => &x.candidate.declaration.identity,
        ProofPhase::Rejected(x) => &x.candidate.declaration.identity,
    }
}
fn checksum(id: Uuid, phase: &ProofPhase) -> Result<[u8; 32], RepositoryError> {
    Ok(Sha256::digest(
        serde_json::to_vec(&(1u32, id, phase)).map_err(|_| RepositoryError::Corrupt)?,
    )
    .into())
}

mod bounded_base64 {
    use super::*;
    use serde::{Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error> {
        STANDARD.encode(value).serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len()
            > MAX_REPLACEMENT_BYTES
                .saturating_mul(4)
                .saturating_div(3)
                .saturating_add(8)
        {
            return Err(serde::de::Error::custom("replacement is oversized"));
        }
        let value = STANDARD.decode(text).map_err(serde::de::Error::custom)?;
        if value.len() > MAX_REPLACEMENT_BYTES {
            return Err(serde::de::Error::custom("replacement is oversized"));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persist_promote_reject_scan_delete() {
        let t = tempfile::tempdir().unwrap();
        let r = ProofRepository::open(t.path()).unwrap();
        let id = DeclarationIdentity {
            library: LogicalLibrary(vec!["L".into()]),
            modules: vec![],
            constant: "t".into(),
        };
        let baseline = TrustBaseline {
            identity: [0; 32],
            explicit_axioms: vec![],
            external_artifacts: vec![],
            forbidden_locals: vec![],
            toolchain: [4; 32],
        };
        let baseline = TrustBaseline {
            identity: baseline_identity(&baseline).unwrap(),
            ..baseline
        };
        let c = SolvedCandidate {
            declaration: OpenDeclaration {
                kind: DeclarationKind::Theorem,
                identity: id.clone(),
                anchor: SourceAnchor {
                    source_digest: [1; 32],
                    normalized_statement: "True".into(),
                    context: vec![],
                    old_body_digest: [2; 32],
                },
            },
            commands: vec![CanonicalTactic("exact I.".into())],
            baseline,
            content_identity: [0; 32],
        };
        let c = SolvedCandidate::new(c.declaration, c.commands, c.baseline).unwrap();
        let _pending = r.persist(c.clone()).unwrap();
        assert_eq!(r.scan().unwrap().len(), 1);
        r.reject(
            &id,
            CandidateRejection::new(
                c,
                RejectionPhase::TrustAudit,
                vec![Diagnostic::new("trust", "rejected").unwrap()],
            )
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            r.scan().unwrap()[&id].phase,
            ProofPhase::Rejected(_)
        ));
        assert!(matches!(
            r.acknowledge_closed(&id),
            Err(RepositoryError::IllegalTransition)
        ));
        assert!(matches!(
            r.scan().unwrap()[&id].phase,
            ProofPhase::Rejected(_)
        ))
    }
    #[test]
    fn replacement_encoding_accepts_one_mebibyte_and_bounds_raw_bytes() {
        assert!(
            FileReplacement::with_original(PathBuf::from("Main.v"), None, vec![7; 1024 * 1024])
                .is_ok()
        );
        assert!(matches!(
            FileReplacement::with_original(
                PathBuf::from("Main.v"),
                None,
                vec![7; MAX_REPLACEMENT_BYTES + 1]
            ),
            Err(RepositoryError::Oversize)
        ));
    }
}
