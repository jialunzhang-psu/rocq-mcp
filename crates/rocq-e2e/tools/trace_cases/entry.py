"""Generator CLI and corpus manifest."""

from __future__ import annotations

import argparse

from .common import write_jsonl
from .matrix import OUTPUT, ROOT, assign_traces, matrices, product
from .feasibility import mark_exclusions
from .fault import materialize_publication_fault
from .simultaneous import materialize_simultaneous_requests
from .start import environment_change_is_materialized, materialize_environment_change, materialize_start_failures, materialize_start_parameters, materialize_start_paths, start_failure_is_materialized
from .query import materialize_query_expression, materialize_query_failures, materialize_search_boundaries, materialize_search_invalid, materialize_search_valid, query_failure_is_materialized
from .check import check_case_is_materialized, check_multi_failure_is_materialized, check_publication_is_materialized, materialize_check, materialize_check_escaping_heads, materialize_check_multi_failures, materialize_check_multi_order, materialize_check_multi_parameters, materialize_check_publication
from .declare import declare_failure_is_materialized, materialize_declare_boundaries, materialize_declare_failures, materialize_declare_parameters
from .multiuser import materialize_three_users, materialize_two_users
from .prove import materialize_prove, materialize_prove_failures, prove_failure_is_materialized
from .target_query import materialize_query_kind, materialize_query_target


def mark_existing(family: str, records: list[dict[str, object]]) -> None:
    """Retain complete generated families not selected for this incremental run.

    A partially materialized family remains explicitly ``unmapped`` (apart
    from any individually mapped legacy records) rather than claiming broad
    coverage from a subset of files.
    """
    if family in {
        "prove",
        "query_target",
        "check",
        "declare_failures",
        "prove_failures",
        "query_failures",
        "check_multi_failures",
        "check_publication",
        "start_failures",
        "environment_change",
    }:
        # These existing directories intentionally cover only deterministic
        # subsets; fault/status setup cases retain their unmapped status.
        for record in records:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if family == "prove" and (
                axes["runtime"] == "alive"
                or (
                    axes["runtime"] in {
                        "pet_killed",
                        "pet_evicted",
                        "proof_timeout",
                    }
                    and axes["target"] not in {"pending", "rejected"}
                )
                or (
                    axes["runtime"] in {"pet_killed", "pet_evicted", "proof_timeout"}
                    and axes["target"] in {"pending", "rejected"}
                )
            ):
                record["implementation"] = "implemented"
            elif family == "query_target":
                record["implementation"] = "implemented"
            elif family == "check" and check_case_is_materialized(axes):
                record["implementation"] = "implemented"
            elif family == "declare_failures" and (declare_failure_is_materialized(axes)
                                                   or (axes["failure"] in {"declaration_changed", "ambiguous_location"}
                                                       and (ROOT / str(record["trace"])).is_file())):
                record["implementation"] = "implemented"
            elif family == "prove_failures" and prove_failure_is_materialized(axes):
                record["implementation"] = "implemented"
            elif family == "query_failures" and query_failure_is_materialized(axes):
                record["implementation"] = "implemented"
            elif family == "check_multi_failures" and check_multi_failure_is_materialized(
                axes
            ):
                record["implementation"] = "implemented"
            elif family == "check_publication" and check_publication_is_materialized(axes):
                record["implementation"] = "implemented"
            elif family == "start_failures" and start_failure_is_materialized(axes):
                record["implementation"] = "implemented"
            elif family == "environment_change" and environment_change_is_materialized(axes):
                record["implementation"] = "implemented"
        return
    targets = {ROOT / str(record["trace"]) for record in records}
    if all(target.is_file() for target in targets):
        for record in records:
            record["implementation"] = "implemented"

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--family",
        action="append",
        help="regenerate this family (repeatable); retain other generated families",
    )
    selected = parser.parse_args().family
    materializers = {
        "start_parameters": materialize_start_parameters,
        "start_paths": materialize_start_paths,
        "start_failures": materialize_start_failures,
        "environment_change": materialize_environment_change,
        "search_valid": materialize_search_valid,
        "search_invalid": materialize_search_invalid,
        "search_boundaries": materialize_search_boundaries,
        "query_kind": materialize_query_kind,
        "query_expression": materialize_query_expression,
        "query_failures": materialize_query_failures,
        "declare_parameters": materialize_declare_parameters,
        "declare_boundaries": materialize_declare_boundaries,
        "declare_failures": materialize_declare_failures,
        "check": materialize_check,
        "check_publication": materialize_check_publication,
        "check_multi_parameters": materialize_check_multi_parameters,
        "check_multi_order": materialize_check_multi_order,
        "check_multi_failures": materialize_check_multi_failures,
        "check_escaping_heads": materialize_check_escaping_heads,
        "two_users": materialize_two_users,
        "three_users": materialize_three_users,
        "prove": materialize_prove,
        "prove_failures": materialize_prove_failures,
        "query_target": materialize_query_target,
        "publication_fault": materialize_publication_fault,
        "simultaneous_requests": materialize_simultaneous_requests,
    }
    for family in selected or []:
        if family not in materializers:
            parser.error(f"family is not materialized: {family}")
    all_records: list[dict[str, object]] = []
    for family, axes in matrices():
        records = product(family, **axes)
        assign_traces(family, records)
        if family in materializers:
            if selected is None or family in selected:
                materializers[family](records)
            else:
                mark_existing(family, records)
        all_records.extend(records)

    mark_exclusions(all_records)

    implemented_paths = {
        str(record["trace"]) for record in all_records
        if record["implementation"] == "implemented"
    }
    missing_paths = sorted(path for path in implemented_paths if not (ROOT / path).is_file())
    if missing_paths:
        raise RuntimeError(f"implemented cases have no trace files: {missing_paths[:8]}")

    slots = [(record["trace"], record["case_index"]) for record in all_records]
    if len(slots) != len(set(slots)):
        raise RuntimeError("two cases occupy the same trace case slot")

    write_jsonl(OUTPUT, all_records, sort_keys=True)
