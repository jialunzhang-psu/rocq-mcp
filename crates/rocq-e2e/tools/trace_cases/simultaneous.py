"""Two live MCP connections issuing genuinely overlapping requests."""

from __future__ import annotations

from .common import PROOF_CATALOG, PROOF_OPEN, event, user_command
from .check import PAIR_OPEN
from .multiuser import close_lost, close_success, finish_users, single_user_catalog, truth_state, user_setup, write_dedicated_family


def _parallel(item: dict[str, object]) -> dict[str, object]:
    """Mark one command as a member of the adjacent two-request barrier."""
    item["parallel_group"] = "race"
    return item


def materialize_simultaneous_requests(records: list[dict[str, object]]) -> None:
    """Emit all four concurrency boundaries with complete output oracles."""
    def build(record: dict[str, object]) -> list[dict[str, object]]:
        axes = record["axes"]
        assert isinstance(axes, dict)
        boundary = axes["boundary"]
        events = [event("server_start"), event("user_connect", user="alice"), event("user_connect", user="bob")]
        if boundary == "same_theorem_double_close":
            for user in ("alice", "bob"):
                user_setup(events, user, "matrix_project", PROOF_CATALOG, "truth", PROOF_OPEN)
            loser = close_lost("bob", "exact I.")
            loser["expected"] = {"$one_of": [
                loser["expected"],
                {"state": {**PROOF_OPEN, "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n"},
                 "error": {"kind": "not_found", "message": "proof attempt is no longer open"}},
            ]}
            events.extend([
                _parallel(close_success("alice", PROOF_OPEN, "exact I.")),
                _parallel(loser),
            ])
        elif boundary == "different_theorem_same_project_close":
            user_setup(events, "alice", "matrix_project", PROOF_CATALOG, "truth", PROOF_OPEN)
            user_setup(events, "bob", "matrix_project", PROOF_CATALOG, "pair", PAIR_OPEN)
            events.extend([
                _parallel(close_success("alice", PROOF_OPEN, "exact I.")),
                _parallel(close_success("bob", PAIR_OPEN, "exact (conj I I).")),
            ])
        elif boundary == "different_project_close":
            a = truth_state("ProjectA.Main.truth_a", "Theorem truth_a : True")
            b = truth_state("ProjectB.Main.truth_b", "Theorem truth_b : True")
            user_setup(events, "alice", "user_project_a", single_user_catalog("a"), "truth_a", a)
            user_setup(events, "bob", "user_project_b", single_user_catalog("b"), "truth_b", b)
            events.extend([
                _parallel(close_success("alice", a, "exact I.")),
                _parallel(close_success("bob", b, "exact I.")),
            ])
        elif boundary == "read_during_close":
            user_setup(events, "alice", "matrix_project", PROOF_CATALOG, "truth", PROOF_OPEN)
            catalog_completed = {"declarations": [
                {**entry, "status": "Completed"} if entry["name"] == "Matrix.Main.truth" else entry
                for entry in PROOF_CATALOG["declarations"]
            ]}
            events.extend([
                _parallel(close_success("alice", PROOF_OPEN, "exact I.")),
                _parallel(user_command("bob", "start", {"project_path": "matrix_project"},
                                       {"$one_of": [PROOF_CATALOG, catalog_completed]})),
                user_command("bob", "start", {"project_path": "matrix_project"}, catalog_completed),
            ])
        else:
            raise AssertionError(boundary)
        finish_users(events, ["alice", "bob"])
        return events

    write_dedicated_family("simultaneous_requests", records, build)
