"""Audited, user-approved exclusions from the finite MCP case product.

An exclusion describes an unreachable *command/error crossing*, never a
successful trace. The row remains in TRACE_CASES.jsonl.zst for future re-audit.
"""

from __future__ import annotations

from collections import Counter


# Each reason is a falsifiable statement about the current public MCP route.
REASONS = {
    "query_without_target": "This query kind has no target name to resolve.",
    "query_without_pet": "This query kind does not call PET, so it cannot time out in PET.",
    "attached_project_query": "A successful start retains project ownership; query reuses that attachment.",
    "attached_project_prove": "A successful start retains project ownership; prove reuses that attachment.",
    "attached_project_check": "Start and declare retain project ownership; check reuses that attachment.",
    "attached_project_try": "An active proof already owns the project attachment used by try.",
    "dune_duplicate_theory_preempts_location": "Dune rejects duplicate Rocq theory names during workspace discovery, before declaration-location resolution is reachable.",
}

EXPECTED_COUNTS = {
    "query_without_target": 12,
    "query_without_pet": 12,
    "attached_project_query": 27,
    "attached_project_prove": 3,
    "attached_project_check": 9,
    "attached_project_try": 3,
    "dune_duplicate_theory_preempts_location": 3,
}


def exclusion_code(family: str, axes: dict[str, object]) -> str | None:
    """Return the single audited reason for an unreachable public outcome."""
    if family == "declare_failures" and axes["layout"] == "dune" and axes["failure"] == "ambiguous_location":
        return "dune_duplicate_theory_preempts_location"
    if family == "query_failures":
        kind, failure = axes["kind"], axes["failure"]
        if failure in {"not_found", "ambiguous"} and kind in {"goals", "search", "type", "notations"}:
            return "query_without_target"
        if failure == "proof_timeout" and kind in {"search", "statement", "proof", "definition"}:
            return "query_without_pet"
        if failure == "project_timeout":
            return "attached_project_query"
    if family == "prove_failures" and axes["failure"] == "project_timeout":
        return "attached_project_prove"
    if family == "check_publication" and axes["outcome"] == "project_timeout":
        return "attached_project_check"
    if family == "try_failures" and axes["failure"] == "project_timeout":
        return "attached_project_try"
    return None


def mark_exclusions(records: list[dict[str, object]]) -> None:
    """Classify exactly the audited unreachable rows; reject drift."""
    counts: Counter[str] = Counter()
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        code = exclusion_code(str(record["family"]), axes)
        if code is None:
            continue
        if record["implementation"] != "unmapped":
            raise RuntimeError(f"exclusion overlaps implemented case: {record['family']} {axes}")
        record["implementation"] = "excluded"
        record["exclusion"] = {"code": code, "reason": REASONS[code]}
        counts[code] += 1
    if counts != EXPECTED_COUNTS:
        raise RuntimeError(f"exclusion matrix changed: {counts} != {EXPECTED_COUNTS}")
