//! Public Rocq engine contracts and facade composition.
//!
//! `engine` orchestrates one project-owned state model, `dune` and `pet` wrap
//! their respective authorities, and `writeback` owns the source CAS. Native
//! proof semantics always come from PET/Rocq.
mod engine;
pub use engine::{Engine, validate_identity};
type Result<T> = std::result::Result<T, Error>;
mod dune;
mod pet;
use types::{CanonicalTactic, DeclarationSource, format_name};
mod types;
mod writeback;

pub use types::{
    AttemptId, AttemptResult, CheckResult, DeclarationIdentity, DeclarationInfo, DeclarationKind,
    EngineConfig, Error, ErrorKind, FileId, LogicalLibrary, ProofLifecycle, ProofState,
};

use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::{
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
        let ranges = pet::sentence_ranges(source).unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(&source[ranges[0].clone()], source.trim_end());
    }

    #[test]
    fn source_edit_scanner_does_not_split_tuple_projections() {
        let source = "Theorem t : (f x).1 = (f x).2. Admitted.";
        let ranges = pet::sentence_ranges(source).unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(&source[ranges[0].clone()], "Theorem t : (f x).1 = (f x).2.");
    }

    #[test]
    fn query_expression_is_only_bounded_before_pet_validation() {
        assert!(engine::validate_native_fragment("I). Admitted. Theorem x : False").is_ok());
        assert!(engine::validate_native_fragment("Nat.add 1 2").is_ok());
    }

    #[test]
    fn pet_commands_preserve_string_literals_and_whitespace() {
        let command = "  exact (String.eqb \"a  b\" \"a b\").  ";
        assert_eq!(pet::canonical_tactic(command).unwrap().0, command.trim());
    }
}
