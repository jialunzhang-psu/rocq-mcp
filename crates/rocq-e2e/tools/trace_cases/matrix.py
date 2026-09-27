"""Finite case matrix and deterministic trace placement."""

from __future__ import annotations

import hashlib
import itertools
import json
from pathlib import Path


# Design note: this module is nested under tools/trace_cases, while artifacts
# belong to the rocq-e2e crate root.
ROOT = Path(__file__).resolve().parents[2]

OUTPUT = ROOT / "TRACE_CASES.jsonl.zst"

MAX_CASES_PER_TRACE = 2_048

LIFECYCLE = ("connected", "reconnect", "restart")

SELECTION = ("no_project", "project", "open", "completed")

EXTRA = ("absent", "present")

# Dune is the sole project/build authority. `_CoqProject` cases were removed
# rather than retained as a compatibility path: the wrapper must fail closed
# instead of silently activating a second source/layout implementation.
LAYOUT = ("dune",)

def product(family: str, **axes: tuple[str, ...]) -> list[dict[str, object]]:
    names = tuple(axes)
    records = []
    for values in itertools.product(*(axes[name] for name in names)):
        assignment = dict(zip(names, values, strict=True))
        canonical = json.dumps(assignment, sort_keys=True, separators=(",", ":"))
        digest = hashlib.sha256((family + canonical).encode()).hexdigest()[:16]
        records.append({"family": family, "case": digest, "axes": assignment})
    return records

def matrices() -> list[tuple[str, dict[str, tuple[str, ...]]]]:
    """Return the frozen finite equivalence classes for every public surface."""
    return [
        (
            "start_parameters",
            {
                "project_path": ("missing", "null", "wrong_type", "empty", "valid"),
                "extra": EXTRA,
                "selection": SELECTION,
                "layout": LAYOUT,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "start_paths",
            {
                "path": (
                    "relative",
                    "absolute",
                    "spaces",
                    "unicode",
                    "dot_segments",
                    "missing",
                    "regular_file",
                    "empty_directory",
                    "malformed_dune",
                ),
                "catalog": ("empty", "single", "mixed_status"),
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "start_failures",
            {
                "failure": (
                    "project_unavailable",
                    "layout_invalid",
                ),
                "source": ("unchanged", "changed_while_disconnected"),
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "search_valid",
            {
                "pattern": ("missing", "true", "equality", "unknown"),
                "at": ("missing", "open_true"),
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "search_invalid",
            {
                "pattern": ("missing", "wrong_type", "empty", "multiple_sentences", "nul"),
                "at": ("missing", "wrong_type", "empty", "invalid"),
                "extra": EXTRA,
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "search_boundaries",
            {
                "pattern": ("missing", "unicode", "at_limit", "over_limit"),
                "at": ("missing", "unicode", "over_limit"),
                "extra": EXTRA,
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "query_kind",
            {
                "kind": (
                    "missing",
                    "null",
                    "wrong_type",
                    "empty",
                    "unknown",
                    "whitespace",
                    "goals",
                    "search",
                    "statement",
                    "proof",
                    "definition",
                    "assumptions",
                    "dependencies",
                    "type",
                    "notations",
                    "GOALS",
                    "SEARCH",
                    "STATEMENT",
                    "PROOF",
                    "DEFINITION",
                    "ASSUMPTIONS",
                    "DEPENDENCIES",
                    "TYPE",
                    "NOTATIONS",
                ),
                "extra": EXTRA,
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "query_target",
            {
                "kind": ("statement", "proof", "definition", "assumptions", "dependencies"),
                "target": (
                    "full_name",
                    "missing",
                ),
                "declaration_kind": ("Theorem", "Lemma", "Definition"),
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "query_expression",
            {
                "kind": ("type", "notations"),
                "expression": (
                    "missing",
                    "null",
                    "wrong_type",
                    "empty",
                    "valid",
                    "unicode",
                    "nul",
                    "multiple_sentences",
                    "unterminated_comment",
                    "at_limit",
                    "over_limit",
                ),
                "extra": EXTRA,
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "query_failures",
            {
                "kind": (
                    "goals",
                    "search",
                    "statement",
                    "proof",
                    "definition",
                    "assumptions",
                    "dependencies",
                    "type",
                    "notations",
                ),
                "failure": (
                    "not_found",
                    "proof_timeout",
                    "project_timeout",
                    "invalid_configuration",
                ),
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "declare_parameters",
            {
                "name": ("missing", "null", "wrong_type", "empty", "valid"),
                "statement": ("missing", "null", "wrong_type", "empty", "valid"),
                "kind": (
                    "missing",
                    "null",
                    "wrong_type",
                    "empty",
                    "Theorem",
                    "Lemma",
                    "Definition",
                    "Axiom",
                ),
                "extra": EXTRA,
                "selection": SELECTION,
                "layout": LAYOUT,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "declare_boundaries",
            {
                "name": (
                    "unicode",
                    "leading_dot",
                    "trailing_dot",
                    "double_dot",
                    "hyphen",
                    "slash",
                    "at_limit",
                    "over_limit",
                ),
                "statement": (
                    "body",
                    "whitespace",
                    "full_header",
                    "mismatched_header",
                    "multiple_sentences",
                    "nul",
                    "unterminated_comment",
                    "unterminated_string",
                    "at_limit",
                    "over_limit",
                ),
                "kind": ("Theorem", "Lemma", "Definition"),
                "extra": EXTRA,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "declare_failures",
            {
                "failure": (
                    "duplicate",
                    "invalid_module_context",
                    "ambiguous_location",
                    "declaration_changed",
                    "logical_library_unavailable",
                ),
                "layout": LAYOUT,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "dune_duplicate_theory",
            {"lifecycle": LIFECYCLE},
        ),
        (
            "prove",
            {
                "target": (
                    "missing_field",
                    "null",
                    "wrong_type",
                    "empty",
                    "full_name",
                    "completed",
                    "not_found",
                    "invalid_syntax",
                    "at_limit",
                    "over_limit",
                ),
                "extra": EXTRA,
                "selection": SELECTION,
                "runtime": ("alive", "pet_killed", "pet_evicted", "proof_timeout"),
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "prove_failures",
            {
                "failure": (
                    "not_found",
                    "proof_timeout",
                    "project_timeout",
                    "invalid_configuration",
                    "closed_attempt",
                ),
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "check",
            {
                "commands": (
                    "missing",
                    "null",
                    "wrong_type",
                    "empty",
                    "whitespace",
                    "missing_dot",
                    "unterminated_comment",
                    "unterminated_string",
                    "unsolved",
                    "solved",
                    "given_up",
                    "partial_failure",
                    "at_limit",
                    "over_limit",
                    "proof_timeout",
                    "build_timeout",
                ),
                "extra": EXTRA,
                "selection": SELECTION,
                "proof_shape": ("True", "conjunction", "forall", "Definition"),
                "declaration_kind": ("Theorem", "Lemma", "Definition"),
                "layout": LAYOUT,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "check_escaping_heads",
            {
                "head": (
                    "Qed",
                    "Defined",
                    "Admitted",
                    "Abort",
                    "Save",
                    "Restart",
                    "Undo",
                    "Undo_all",
                    "Back",
                    "Reset",
                    "Reset_Initial",
                    "Drop",
                    "Focus",
                    "Unfocus",
                    "Show",
                    "Show_Proof",
                    "Show_Script",
                    "Guarded",
                    "Proof",
                    "Theorem",
                    "Lemma",
                    "Definition",
                    "Fixpoint",
                    "CoFixpoint",
                ),
                "extra": EXTRA,
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "check_publication",
            {
                "outcome": (
                    "open",
                    "completed",
                    "build_timeout",
                    "unfinished_dependency",
                    "declaration_changed",
                    "proof_timeout",
                    "project_timeout",
                    "invalid_configuration",
                ),
                "declaration_kind": ("Theorem", "Lemma", "Definition"),
                "layout": LAYOUT,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "try_parameters",
            {
                "attempts": (
                    "missing",
                    "null",
                    "wrong_container",
                    "wrong_item",
                    "empty_item",
                    "two_sentences",
                    "count_0",
                    "count_1",
                    "count_2",
                    "count_3",
                    "count_4",
                    "count_5",
                    "count_6",
                    "count_7",
                    "count_8",
                    "count_9",
                    "count_10",
                    "count_11",
                    "count_12",
                    "count_13",
                    "count_14",
                    "count_15",
                    "count_16",
                    "count_17",
                    "count_18",
                    "count_19",
                    "count_20",
                    "count_21",
                    "at_limit",
                    "over_limit",
                    "duplicate",
                    "mixed",
                ),
                "extra": EXTRA,
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "try_order",
            {
                "order": (
                    "unsolved_failed_solved",
                    "unsolved_solved_failed",
                    "failed_unsolved_solved",
                    "failed_solved_unsolved",
                    "solved_unsolved_failed",
                    "solved_failed_unsolved",
                ),
                "extra": EXTRA,
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "try_failures",
            {
                "failure": (
                    "declaration_changed",
                    "proof_timeout",
                    "project_timeout",
                    "invalid_configuration",
                ),
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "two_users",
            {
                "variant": (
                    "same_theorem_alice_wins",
                    "same_theorem_bob_wins",
                    "different_theorem_same_file",
                    "different_file_same_project",
                    "different_project",
                ),
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "three_users",
            {
                "variant": (
                    "all_same_alice_wins",
                    "all_same_bob_wins",
                    "all_same_carol_wins",
                    "all_different_same_file",
                    "different_files_same_project",
                    "different_projects",
                    "alice_bob_same_alice_wins",
                    "alice_bob_same_bob_wins",
                    "alice_carol_same_alice_wins",
                    "alice_carol_same_carol_wins",
                    "bob_carol_same_bob_wins",
                    "bob_carol_same_carol_wins",
                ),
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "environment_change",
            {
                "change": (
                    "source_same_mtime",
                    "source_added",
                    "source_deleted",
                    "source_restored",
                    "dune_modules",
                    "configuration_changed",
                ),
                "recovery": ("same_connection", "reconnect", "restart"),
            },
        ),
        (
            "publication_fault",
            {
                "point": (
                    "after_promote",
                    "before_replace",
                    "between_replacements",
                    "after_replace",
                    "after_final_build",
                    "before_ack",
                ),
                "replacement": ("single_file", "multi_file"),
                "recovery": ("same_user_retry", "reconnect_retry", "restart_retry"),
                "response": ("lost",),
            },
        ),
        (
            "simultaneous_requests",
            {
                "boundary": (
                    "same_theorem_double_close",
                    "different_theorem_same_file_close",
                    "different_file_same_project_close",
                    "different_project_close",
                    "read_during_close",
                ),
            },
        ),
    ]

GROUP_AXES = {
    "start_parameters": ("selection", "layout", "lifecycle"),
    "start_paths": ("lifecycle",),
    "start_failures": ("failure", "lifecycle"),
    "search_valid": ("selection", "lifecycle"),
    "search_invalid": ("selection", "lifecycle"),
    "search_boundaries": ("selection", "lifecycle"),
    "query_kind": ("selection", "lifecycle"),
    "query_target": ("selection", "lifecycle"),
    "query_expression": ("selection", "lifecycle"),
    "query_failures": ("kind", "lifecycle"),
    "declare_parameters": ("selection", "layout", "lifecycle"),
    "declare_boundaries": ("kind", "lifecycle"),
    "declare_failures": ("layout", "lifecycle"),
    "dune_duplicate_theory": ("lifecycle",),
    "prove": ("selection", "runtime", "lifecycle"),
    "prove_failures": ("lifecycle",),
    "check": ("selection", "layout", "lifecycle"),
    "check_escaping_heads": ("selection", "lifecycle"),
    "try_parameters": ("selection", "lifecycle"),
    "try_order": ("selection", "lifecycle"),
    "try_failures": ("lifecycle",),
    "check_publication": ("outcome", "layout", "lifecycle"),
}

DEDICATED = {
    "two_users",
    "three_users",
    "environment_change",
    "publication_fault",
    "simultaneous_requests",
}

FIXTURE = {
    "start_parameters": "basic",
    "start_paths": "basic",
    "start_failures": "basic",
    "search_valid": "basic",
    "search_invalid": "basic",
    "search_boundaries": "basic",
    "query_kind": "basic",
    "query_target": "basic",
    "query_expression": "basic",
    "query_failures": "basic",
    "declare_parameters": "proof",
    "declare_boundaries": "proof",
    "declare_failures": "proof",
    "dune_duplicate_theory": "ambiguous_library",
    "prove": "proof",
    "prove_failures": "basic",
    "check": "proof",
    "check_escaping_heads": "proof",
    "check_publication": "proof",
    "try_parameters": "proof",
    "try_order": "proof",
    "try_failures": "proof",
    "two_users": "proof",
    "three_users": "proof",
    "environment_change": "source_change",
    "publication_fault": "fault",
    "simultaneous_requests": "proof",
}

def assign_traces(family: str, records: list[dict[str, object]]) -> None:
    """Assign each case to one trace shard and one case position."""
    groups: dict[str, list[dict[str, object]]] = {}
    keys = GROUP_AXES.get(family, ())
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if family in DEDICATED:
            group = str(record["case"])
        else:
            selected = {key: axes[key] for key in keys}
            group = hashlib.sha256(
                json.dumps(selected, sort_keys=True).encode()
            ).hexdigest()[:12]
        groups.setdefault(group, []).append(record)
    for group, members in groups.items():
        for offset, record in enumerate(members):
            shard = offset // MAX_CASES_PER_TRACE
            fixture = FIXTURE[family]
            # Design note: boundary strings dominate the repository size but
            # remain ordinary replayable JSONL after streaming decompression.
            suffix = (
                ".jsonl.zst"
                if family in {"search_boundaries", "declare_boundaries", "query_expression"}
                else ".jsonl"
            )
            record["trace"] = (
                f"traces/{fixture}/generated/{family}/{group}__{shard:03}{suffix}"
            )
            record["case_index"] = offset % MAX_CASES_PER_TRACE
            record["implementation"] = "unmapped"
    if family == "check":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if check_case_needs_build_timeout_fixture(axes):
                # Environment faults are isolation boundaries: each timeout
                # owns a fresh wrapper marker and disposable project.
                record["trace"] = (
                    "traces/check_timeout/generated/check_build_timeout/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
    if family == "check_publication":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["outcome"] == "build_timeout":
                record["trace"] = (
                    "traces/check_timeout/generated/check_publication/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["outcome"] == "declaration_changed":
                record["trace"] = str(record["trace"]).replace(
                    "traces/proof/", "traces/declaration_change/", 1
                )
            elif axes["outcome"] == "unfinished_dependency":
                record["trace"] = str(record["trace"]).replace(
                    "traces/proof/", "traces/publication_rejected/", 1
                )
            elif axes["outcome"] == "invalid_configuration":
                record["trace"] = (
                    "traces/source_change/generated/check_publication/"
                    f"check_publication_invalid_configuration__{axes['declaration_kind']}__"
                    f"{axes['layout']}__{axes['lifecycle']}.jsonl"
                )
                record["case_index"] = 0
    if family == "prove":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["runtime"] == "pet_killed":
                record["trace"] = (
                    "traces/pet_fault/generated/prove_pet_killed/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["runtime"] == "pet_evicted":
                record["trace"] = (
                    "traces/pet_eviction/generated/prove_pet_evicted/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["runtime"] == "proof_timeout":
                group = f"{axes['selection']}_{axes['lifecycle']}"
                record["trace"] = (
                    "traces/pet_timeout/generated/prove_proof_timeout/"
                    f"{group}/{record['case']}.jsonl"
                )
                record["case_index"] = 0
    if family == "prove_failures":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["failure"] == "proof_timeout":
                record["trace"] = (
                    "traces/pet_timeout/generated/prove_failures/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["failure"] == "invalid_configuration":
                record["trace"] = (
                    "traces/source_change/generated/prove_failures/"
                    f"prove_invalid_configuration__{axes['lifecycle']}.jsonl"
                )
                record["case_index"] = 0
    if family == "query_failures":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["failure"] == "declaration_changed" and axes["kind"] == "goals":
                suffix = "" if axes["lifecycle"] == "connected" else f"__{axes['lifecycle']}"
                change = "source_same_mtime_late" if axes["lifecycle"] == "restart" else "source_same_mtime"
                record["trace"] = (
                    "traces/source_change/generated/query_failures/"
                    f"{change}__open__same_connection__query_goals_declaration_changed{suffix}.jsonl"
                )
                record["case_index"] = 0
                continue
            if axes["failure"] == "invalid_configuration":
                record["trace"] = (
                    "traces/source_change/generated/query_failures/"
                    f"query_invalid_configuration__{axes['kind']}__{axes['lifecycle']}.jsonl"
                )
                record["case_index"] = 0
                continue
            if axes["failure"] == "proof_timeout" and axes["kind"] in {
                "goals",
                "type",
                "notations",
                "assumptions",
                "dependencies",
            }:
                record["trace"] = (
                    "traces/pet_timeout/generated/query_failures/"
                    f"{'open_' if axes['kind'] == 'goals' else ''}{axes['lifecycle']}_"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
    if family == "environment_change":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            fixture = "source_change"
            record["trace"] = (
                f"traces/{fixture}/generated/environment_change/"
                f"{axes['change']}__open__{axes['recovery']}.jsonl"
            )
            record["case_index"] = 0
    if family == "try_failures":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["failure"] == "invalid_configuration":
                record["trace"] = (
                    "traces/source_change/generated/try_failures/"
                    f"try_invalid_configuration__{axes['lifecycle']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["failure"] == "declaration_changed":
                suffix = "" if axes["lifecycle"] == "connected" else f"__{axes['lifecycle']}"
                change = "source_same_mtime_late" if axes["lifecycle"] == "restart" else "source_same_mtime"
                record["trace"] = (
                    "traces/source_change/generated/try_failures/"
                    f"{change}__open__same_connection__try_failures{suffix}.jsonl"
                )
                record["case_index"] = 0
    if family == "declare_failures":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["failure"] == "declaration_changed":
                record["trace"] = (
                    "traces/declare_race/generated/declare_failures/"
                    f"{axes['layout']}__{axes['lifecycle']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["failure"] == "ambiguous_location":
                record["trace"] = (
                    "traces/ambiguous_library/generated/declare_failures/"
                    f"{axes['layout']}__{axes['lifecycle']}.jsonl"
                )
                record["case_index"] = 0
    if family == "publication_fault":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            record["trace"] = (
                "traces/fault/generated/publication_fault/"
                f"{axes['point']}__{axes['replacement']}__{axes['recovery']}.jsonl"
            )
            record["case_index"] = 0
def check_case_needs_build_timeout_fixture(axes: dict[str, object]) -> bool:
    return (
        axes["commands"] == "build_timeout"
        and axes["selection"] == "open"
        and axes["extra"] == "absent"
    )
