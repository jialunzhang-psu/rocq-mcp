---
name: rocq-spec-review
description: >
  Facts about Rocq declaration surfaces, assumptions reports, proof-free
  documentation views, and source-level specification extraction.
---

# Rocq Specification Facts

- A theorem or lemma declaration has a type that is independent of the tactic
  script used to construct its proof term.
- `Print Assumptions target.` reports the assumptions in the target's logical
  dependency closure.
- The assumptions report does not enumerate every proved intermediate lemma,
  every source-level hole, or declarations that are not reachable from the
  target.
- An assumptions report does not compare a theorem statement with an informal
  intended specification.
- `rocq doc --raw --light --stdout file.v` emits a proof-free documentation
  view of the declarations in `file.v`.
- The repository script `extract_spec_view.py` invokes that command for selected
  `.v` files and writes the resulting views while preserving their relative
  source paths.
- The script accepts a repository path and output directory, optional
  `--files-from`, `--project-file`, `--no-auto-project-file`, `--clean-output`,
  repeated `--exclude-dir`, and `--include-hidden` arguments, together with
  additional Rocq arguments.
- Declaration surfaces include theorem and lemma statements, definition bodies,
  inductive types, records, classes, modules, notation declarations, typeclass
  declarations, and explicit assumptions.
- A declaration present only in a separate worktree is not automatically a
  dependency of a declaration in the main worktree.
