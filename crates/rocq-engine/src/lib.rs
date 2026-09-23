//! Public Rocq engine contracts and facade composition.
//!
//! Source spans, temporary PET documents, interactive replay, typed queries,
//! process lifecycle, project attachment, and durable records each have one
//! module owner. Native proof semantics always come from PET/Rocq.
mod api;
mod candidate;
mod engine;
mod engine_runtime;
pub use api::*;
pub use engine::Engine;
use engine::{
    trace_error, valid_identifier, validate_identity, validate_name, validate_native_fragment,
    validate_search,
};
type Result<T> = std::result::Result<T, Error>;
const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
mod layout;
mod native_process;
mod pet_document;
mod pet_runtime;
pub(crate) use pet_document::{PetDocumentSpec, pet_document_spec, read_source};
mod source_index;
use source_index::{
    ScopeEvent, SourceDeclaration, context_insertion_offset, context_occurrences,
    declaration_header, discover, format_name, local_name, parse_file, resolve_declaration,
    scope_event, scope_name,
};
mod source_spans;
use source_spans::{
    canonical_tactic, lexical_words, normalize_fragment, normalize_sentence, sentence_ranges,
};
mod project_state;
mod publication;
mod query;
mod repository;

pub use repository::{
    CandidateRejection, CanonicalTactic, ClosedProof, DeclarationIdentity, DeclarationKind,
    Diagnostic, FileReplacement, LexicalScope, LogicalLibrary, NewDeclaration, OpenDeclaration,
    ProofTerminator, RejectionPhase, SolvedCandidate, SourceAnchor, TrustAuditResult,
    TrustBaseline,
};
use repository::{PendingRecord, ProofPhase, RepositoryError};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    ops::Range,
    path::{Path, PathBuf},
    time::Duration,
};
use trace_forest::{ActionKey, Config as ForestConfig, CursorId, RootKey, TraceForest};

#[cfg(test)]
extern crate self as rocq_engine;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_handles_nested_comments_doubled_strings_unicode_and_qualified_names() {
        let source = "(* a (* nested. *) comment *) Definition λ.x := \"a.dot \"\" quoted\"\"\".\n";
        let ranges = sentence_ranges(source).unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            normalize_sentence(&source[ranges[0].clone()]),
            "Definition λ.x := \"a.dot \"\" quoted\"\"\""
        );
    }

    #[test]
    fn parser_tracks_scopes_and_proof_lifecycles() {
        let source = "Module Outer. Section S. Module Inner. Theorem λ : True. Proof. exact I. Qed. End Inner. End S. End Outer. Definition d : True := I. Theorem a : True. Admitted.";
        let parsed = parse_file(source, &LogicalLibrary(vec!["Main".into()])).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].info.identity.modules, vec!["Outer", "Inner"]);
        assert_eq!(parsed[0].info.context.len(), 3);
        assert_eq!(parsed[0].info.status, ProofLifecycle::Completed);
        assert_eq!(parsed[1].info.status, ProofLifecycle::Completed);
        assert_eq!(parsed[2].info.status, ProofLifecycle::Open);
        assert_ne!(parsed[0].anchor.source_digest, [0; 32]);
        assert_ne!(parsed[0].anchor.old_body_digest, [0; 32]);
    }

    #[test]
    fn module_type_and_alias_do_not_create_scopes() {
        let source = "Module Type API. End API. Module Import P := API. Module Impl. Theorem t : True. Admitted. End Impl.";
        let parsed = parse_file(source, &LogicalLibrary(vec!["Main".into()])).unwrap();
        assert_eq!(parsed[0].info.identity.modules, vec!["Impl"]);
    }

    #[test]
    fn query_expression_rejects_multiple_sentences() {
        assert!(validate_native_fragment("I). Admitted. Theorem x : False").is_err());
        assert!(validate_native_fragment("Nat.add 1 2").is_ok());
    }
}

#[cfg(test)]
mod layout_black_box_tests;
#[cfg(test)]
mod project_state_black_box_tests;
#[cfg(test)]
mod repository_black_box_tests;
