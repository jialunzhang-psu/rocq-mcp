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

LAYOUT = ("coqproject", "dune")

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
                    "ambiguous_layout",
                    "malformed_coqproject",
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
                    "layout_ambiguous",
                    "project_lock_timeout",
                ),
                "source": ("unchanged", "changed_while_disconnected"),
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "search_valid",
            {
                "name": ("missing", "Main", "duplicate", "done_true", "missing_name"),
                "statement": ("missing", "True", "nat"),
                "status": ("missing", "Open", "Completed", "Pending", "Rejected"),
                "offset": ("zero", "one"),
                "limit": ("one", "twenty"),
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "search_invalid",
            {
                "name": ("missing", "wrong_type", "empty", "invalid"),
                "statement": ("missing", "wrong_type", "empty", "invalid"),
                "status": ("missing", "wrong_type", "empty", "invalid"),
                "offset": ("missing", "wrong_type", "negative", "fractional"),
                "limit": ("missing", "wrong_type", "zero", "over_max", "negative"),
                "extra": EXTRA,
                "selection": SELECTION,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "search_boundaries",
            {
                "name": ("missing", "unicode", "at_limit", "over_limit"),
                "statement": ("missing", "unicode", "at_limit", "over_limit"),
                "status": ("missing", "exact", "lowercase", "unknown"),
                "offset": (
                    "missing",
                    "zero",
                    "one",
                    "negative",
                    "fractional",
                    "string",
                    "uint_max",
                ),
                "limit": (
                    "missing",
                    "one",
                    "twenty",
                    "hundred",
                    "zero",
                    "over_max",
                    "fractional",
                    "string",
                ),
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
                    "open_short",
                    "completed_short",
                    "module_suffix",
                    "full_name",
                    "missing",
                    "ambiguous",
                    "pending",
                    "rejected",
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
                    "ambiguous",
                    "declaration_changed",
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
                    "state_directory_unavailable",
                ),
                "layout": LAYOUT,
                "lifecycle": LIFECYCLE,
            },
        ),
        (
            "prove",
            {
                "target": (
                    "missing_field",
                    "null",
                    "wrong_type",
                    "empty",
                    "open_short",
                    "nested_suffix",
                    "library_suffix",
                    "full_name",
                    "completed",
                    "pending",
                    "rejected",
                    "not_found",
                    "ambiguous",
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
                    "ambiguous",
                    "declaration_changed",
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
                    "pending_build_timeout",
                    "rejected_unfinished_dependency",
                    "rejected_axiom_out_of_scope",
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
            "check_multi_parameters",
            {
                "candidates": (
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
            "check_multi_order",
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
            "check_multi_failures",
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
                    "load_path",
                    "direct_vo_changed",
                    "direct_vo_deleted",
                    "transitive_vo_changed",
                    "plugin_vo_changed",
                    "rocq_identity_changed",
                    "pet_identity_changed",
                    "configuration_changed",
                    "axiom_after_baseline",
                ),
                "candidate": ("open", "solved_pending"),
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
                    "different_theorem_same_project_close",
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
    "prove": ("selection", "runtime", "lifecycle"),
    "prove_failures": ("lifecycle",),
    "check": ("selection", "layout", "lifecycle"),
    "check_escaping_heads": ("selection", "lifecycle"),
    "check_multi_parameters": ("selection", "lifecycle"),
    "check_multi_order": ("selection", "lifecycle"),
    "check_multi_failures": ("lifecycle",),
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
    "prove": "proof",
    "prove_failures": "basic",
    "check": "proof",
    "check_escaping_heads": "proof",
    "check_publication": "proof",
    "check_multi_parameters": "proof",
    "check_multi_order": "proof",
    "check_multi_failures": "proof",
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
            if axes["outcome"] == "pending_build_timeout":
                record["trace"] = (
                    "traces/check_timeout/generated/check_publication/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["outcome"] == "declaration_changed":
                record["trace"] = str(record["trace"]).replace(
                    "traces/proof/", "traces/declaration_change/", 1
                )
            elif axes["outcome"] == "rejected_axiom_out_of_scope":
                record["trace"] = str(record["trace"]).replace(
                    "traces/proof/", "traces/axiom_injection/", 1
                )
            elif axes["outcome"] == "rejected_unfinished_dependency":
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
    if family == "query_target":
        status_groups: dict[tuple[str, str, str, str], list[dict[str, object]]] = {}
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["target"] in {"pending", "rejected"}:
                key = (
                    str(axes["target"]),
                    str(axes["declaration_kind"]),
                    str(axes["selection"]),
                    str(axes["lifecycle"]),
                )
                status_groups.setdefault(key, []).append(record)
        for key, members in status_groups.items():
            status, declaration_kind, selection, lifecycle = key
            fixture = "check_timeout" if status == "pending" else "query_rejected"
            digest = hashlib.sha256("|".join(key).encode()).hexdigest()[:12]
            for index, record in enumerate(members):
                record["trace"] = (
                    f"traces/{fixture}/generated/query_target_{status}/"
                    f"{digest}.jsonl"
                )
                record["case_index"] = index
    if family == "prove":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["runtime"] == "alive" and axes["target"] in {
                "pending",
                "rejected",
            }:
                status = str(axes["target"])
                fixture = "check_timeout" if status == "pending" else "query_rejected"
                record["trace"] = (
                    f"traces/{fixture}/generated/prove_{status}/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["runtime"] != "alive" and axes["target"] in {
                "pending",
                "rejected",
            }:
                runtime = str(axes["runtime"])
                record["trace"] = (
                    f"traces/status_runtime/generated/prove_{runtime}/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["runtime"] == "pet_killed" and axes["target"] not in {
                "pending",
                "rejected",
            }:
                record["trace"] = (
                    "traces/pet_fault/generated/prove_pet_killed/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["runtime"] == "pet_evicted" and axes["target"] not in {
                "pending",
                "rejected",
            }:
                record["trace"] = (
                    "traces/pet_eviction/generated/prove_pet_evicted/"
                    f"{record['case']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["runtime"] == "proof_timeout" and axes["target"] not in {
                "pending",
                "rejected",
            }:
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
            elif axes["failure"] == "declaration_changed":
                recovery = "same_connection" if axes["lifecycle"] == "connected" else axes["lifecycle"]
                # Design note: one process trace witnesses both the environment
                # transition and the public prove error, without duplicate labs.
                record["trace"] = (
                    "traces/source_change/generated/environment_change/"
                    f"source_same_mtime__solved_pending__{recovery}.jsonl"
                )
                record["case_index"] = 1
            elif axes["failure"] == "invalid_configuration":
                record["trace"] = (
                    "traces/source_change/generated/prove_failures/"
                    f"prove_invalid_configuration__{axes['lifecycle']}.jsonl"
                )
                record["case_index"] = 0
    if family == "start_failures":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["failure"] == "project_lock_timeout":
                record["trace"] = (
                    "traces/project_timeout/generated/start_failures/"
                    f"{record['case']}.jsonl"
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
            fixture = "toolchain_change" if axes["change"] in {
                "rocq_identity_changed", "pet_identity_changed"
            } else "source_change"
            record["trace"] = (
                f"traces/{fixture}/generated/environment_change/"
                f"{axes['change']}__{axes['candidate']}__{axes['recovery']}.jsonl"
            )
            record["case_index"] = 0
    if family == "check_multi_failures":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["failure"] == "invalid_configuration":
                record["trace"] = (
                    "traces/source_change/generated/check_multi_failures/"
                    f"check_multi_invalid_configuration__{axes['lifecycle']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["failure"] == "declaration_changed":
                suffix = "" if axes["lifecycle"] == "connected" else f"__{axes['lifecycle']}"
                change = "source_same_mtime_late" if axes["lifecycle"] == "restart" else "source_same_mtime"
                record["trace"] = (
                    "traces/source_change/generated/check_multi_failures/"
                    f"{change}__open__same_connection__check_multi_failures{suffix}.jsonl"
                )
                record["case_index"] = 0
    if family == "declare_failures":
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["failure"] == "state_directory_unavailable":
                record["trace"] = (
                    "traces/proof/generated/declare_state_unavailable/"
                    f"{axes['layout']}__{axes['lifecycle']}.jsonl"
                )
                record["case_index"] = 0
            elif axes["failure"] == "declaration_changed":
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
