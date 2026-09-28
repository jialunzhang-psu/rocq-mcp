---
name: rocq-spec-review
description: Review Rocq/Coq theorem specifications, declarations, definitions, and module structure without proof scripts. For large-project proof-status or strategy questions, start from the target theorem's Print Assumptions and inspect relevant contracts on demand; use rocq doc --raw --light for scoped proof-free views.
---

# Rocq Spec Review

Use this skill to understand theorem contracts and declaration structure. Choose the review scope before extracting or reading specifications; a whole-repository spec view is not a prerequisite.

## Choose the Review Scope

- **A particular statement or small module:** inspect its declaration and the definitions needed to interpret it, using a scoped proof-free view or focused Rocq queries.
- **Remaining holes, proof strategy, or worker priorities in a large project:** use the target-first workflow below. Do not begin by extracting and reading every lemma in every worktree.
- **An explicitly requested repository-wide specification review:** extract a broader view, then read it in coherent module groups. Report which groups were reviewed.

When a file contains too many declarations to review usefully, narrow by theorem name and source location instead of reading the entire generated file. Use `Check` or `About` for theorem statements, `Print` for relevant definitions, and `rg` for navigation. Printing a theorem's proof term is not a specification review.

## Target-First Review for Large Projects

1. Identify the exact target theorem and repository/worktree revision. In its current project environment, obtain the complete `Print Assumptions target.` report, preferably through Rocq MCP or an existing project audit. Use a matching compiled artifact; if the target cannot be loaded or built, report that limitation and fall back to source-level inspection without claiming a verified dependency count.
2. Separate unfinished project obligations from intentionally accepted logical axioms. Preserve fully qualified names when counting or mapping dependencies. Do not count from `head`, a truncated tool response, or a summary: request complete output or use a full captured audit report first.
3. Locate the relevant obligations with `rg`. Group them by shared contract and dependencies, then inspect the target statement, selected obligation statements, and definitions needed to understand their premises and conclusions. Generate only those files' proof-free views when useful.
4. Follow already-proved helpers or consumers only when needed to establish a connection, assess a proposed replacement, or resolve a specific uncertainty. Do not recursively read every proved lemma merely because it is in the repository.
5. For worktree reviews, inspect changed and untracked deliverables separately against their base and the main tree. Their absence from the main theorem's assumptions may mean they are not integrated, not that they are irrelevant. A `Qed` in a worktree is not by itself an accepted main-tree result.

`Print Assumptions` is a dependency frontier, not a complete lemma graph or a substitute for specification review. It omits proved intermediate lemmas and unreferenced holes; it also does not check whether the theorem statement matches the intended specification. A closed report is not evidence that every file is hole-free. For a whole-project hole count, compare the report with a source-level inventory and explain any differences.

For a status-only question, the report plus focused declarations may be enough; no spec extraction is required. For an invariant redesign, read the complete relevant contracts and definitions before proposing changes. Use `rocq-axiom-audit` for trust claims and `rocq-repl-workflow` for interactive proof or invariant experiments when those skills are available.

## Scoped Proof-Free Extraction

The existing extractor accepts `--files-from`, a newline-delimited list of selected `.v` paths relative to the repository (or absolute paths):

```bash
python3 /path/to/rocq-spec-review/scripts/extract_spec_view.py <repo> <output-dir> \
  --files-from <selected-files.txt>
```

Prepare this list from the review scope, and check that the generated files match it. For example, select the module containing the target and the modules defining the obligations being investigated, not every file in `Proof/`. Keep the file list outside the output directory. Omit `--files-from` only when extraction of the whole selected repository or source directory is warranted.

The script runs this command for each selected source file:

```bash
rocq doc --raw --light --stdout <file.v>
```

Read the relevant declarations in the `.v` files under `<output-dir>` as the primary review surface. The output directory should contain only generated `.v` files, preserving the selected source tree layout but omitting proof scripts.

Treat `<output-dir>` as a read-only temporary review view. Do not edit, compile, run `rocq compile`, run project builds, or make semantic conclusions from executing commands inside it. For any execution, compilation, proof repair, or source edit, return to the original `<repo>`.

## Extraction Workflow

1. Choose a unique temporary output directory outside the source tree when possible.
2. Run `scripts/extract_spec_view.py <repo> <output-dir> --files-from <selected-files.txt>` for a scoped review.
3. Read the relevant generated declarations, expanding to other contracts only as the review question requires.
4. Read original proof scripts only after the spec-level review needs proof internals, tactic behavior, or local proof-only facts.
5. After the spec review is done, delete `<output-dir>` unless the user explicitly asked to keep it.

For projects requiring load-path flags, pass a project file or extra Rocq args:

```bash
python3 /path/to/rocq-spec-review/scripts/extract_spec_view.py <repo> <output-dir> \
  --files-from <selected-files.txt> \
  --project-file <repo>/_RocqProject

python3 /path/to/rocq-spec-review/scripts/extract_spec_view.py <repo> <output-dir> \
  --files-from <selected-files.txt> \
  -- -Q <repo>/theories MyProject
```

If extraction fails, use the terminal error output to rerun with the needed `-Q`, `-R`, or `-I` arguments. For Dune projects or uncertain load paths, use `rocq-dune-loadpath` to recover the right project environment.

## Review Focus

When reading the extracted view, focus on:

- theorem, lemma, proposition, and corollary statements;
- definition bodies that are part of the spec;
- inductive, record, class, module, notation, and typeclass surfaces;
- assumptions introduced by `Axiom`, `Parameter`, `Conjecture`, or declared variables.

This skill creates a spec review view only. For trust or soundness claims, also use `rocq-axiom-audit` and check `Print Assumptions` for the relevant theorems.
