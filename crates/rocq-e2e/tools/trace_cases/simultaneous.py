"""Two live MCP connections issuing genuinely overlapping requests."""

from __future__ import annotations

from .common import ATTACHED, PROOF_OPEN, event, user_command
from .check import PAIR_OPEN
from .multiuser import close_lost, close_success, finish_users, truth_state, user_setup, write_dedicated_family


def _parallel(item: dict[str, object]) -> dict[str, object]:
    """Mark one command as a member of the adjacent two-request barrier."""
    item["parallel_group"] = "race"
    return item


def _same_file_close(
    user: str, state: dict[str, object], tactic: str
) -> dict[str, object]:
    """Allow either winner or stale-CAS result without accepting a stale checkpoint."""
    item = close_success(user, state, tactic)
    success = item["expected"]
    item["expected"] = {
        "$one_of": [
            success,
            {
                "kind": "declaration_changed",
                "message": "declaration source changed while proof was open",
            },
            {
                "selected": 0,
                "state": {
                    **state,
                    "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n",
                },
                "rejected": [],
                "error": {
                    "kind": "declaration_changed",
                    "message": "invalid PET request: declaration interface changed",
                },
            },
        ]
    }
    return _parallel(item)


def materialize_simultaneous_requests(records: list[dict[str, object]]) -> None:
    """Emit all four concurrency boundaries with complete output oracles."""
    def build(record: dict[str, object]) -> list[dict[str, object]]:
        axes = record["axes"]
        assert isinstance(axes, dict)
        boundary = axes["boundary"]
        events = [event("server_start"), event("user_connect", user="alice"), event("user_connect", user="bob")]
        if boundary == "same_theorem_double_close":
            for user in ("alice", "bob"):
                user_setup(events, user, "matrix_project", "Matrix.Main.truth", "Main.v", PROOF_OPEN)
            loser = close_lost("bob", "exact I.")
            loser["expected"] = {"$one_of": [
                loser["expected"],
                {
                    "selected": 0,
                    "state": {
                        **PROOF_OPEN,
                        "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n",
                    },
                    "rejected": [],
                    "error": {
                        "kind": "not_found",
                        "message": "proof attempt is no longer open",
                    },
                },
            ]}
            events.extend([
                _parallel(close_success("alice", PROOF_OPEN, "exact I.")),
                _parallel(loser),
            ])
        elif boundary == "different_theorem_same_file_close":
            user_setup(events, "alice", "matrix_project", "Matrix.Main.truth", "Main.v", PROOF_OPEN)
            user_setup(events, "bob", "matrix_project", "Matrix.Main.pair", "Main.v", PAIR_OPEN)
            events.extend([
                _same_file_close("alice", PROOF_OPEN, "exact I."),
                _same_file_close("bob", PAIR_OPEN, "exact (conj I I)."),
            ])
        elif boundary == "different_file_same_project_close":
            a = truth_state("Files.A.a", "Theorem a : True")
            b = truth_state("Files.B.b", "Theorem b : True")
            user_setup(events, "alice", "user_files_project", "Files.A.a", "A.v", a)
            user_setup(events, "bob", "user_files_project", "Files.B.b", "B.v", b)
            events.extend([
                _parallel(close_success("alice", a, "exact I.")),
                _parallel(close_success("bob", b, "exact I.")),
            ])
        elif boundary == "different_project_close":
            a = truth_state("ProjectA.Main.truth_a", "Theorem truth_a : True")
            b = truth_state("ProjectB.Main.truth_b", "Theorem truth_b : True")
            user_setup(events, "alice", "user_project_a", "ProjectA.Main.truth_a", "Main.v", a)
            user_setup(events, "bob", "user_project_b", "ProjectB.Main.truth_b", "Main.v", b)
            events.extend([
                _parallel(close_success("alice", a, "exact I.")),
                _parallel(close_success("bob", b, "exact I.")),
            ])
        elif boundary == "read_during_close":
            user_setup(events, "alice", "matrix_project", "Matrix.Main.truth", "Main.v", PROOF_OPEN)
            events.extend([
                _parallel(close_success("alice", PROOF_OPEN, "exact I.")),
                _parallel(user_command("bob", "start", {"project_path": "matrix_project"}, ATTACHED)),
                user_command("bob", "start", {"project_path": "matrix_project"}, ATTACHED),
            ])
        else:
            raise AssertionError(boundary)
        finish_users(events, ["alice", "bob"])
        return events

    write_dedicated_family("simultaneous_requests", records, build)
