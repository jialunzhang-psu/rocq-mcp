"""Multi-user ordering scenarios."""

from __future__ import annotations

import json
import shutil

from .matrix import FIXTURE, ROOT
from .common import OPEN_TRUE, PROOF_CATALOG, PROOF_OPEN, event, user_command
from .check import PAIR_OPEN
from .declare import FILES_CATALOG


def single_user_catalog(letter: str) -> dict[str, object]:
    return {
        "declarations": [
            {
                "name": f"Project{letter.upper()}.Main.truth_{letter}",
                "statement": f"Theorem truth_{letter} : True",
                "status": "Open",
            }
        ]
    }

def truth_state(theorem: str, statement: str, completed: bool = False) -> dict[str, object]:
    return {
        "theorem": theorem,
        "statement": statement,
        "status": "Completed" if completed else "Open",
        "goals": "" if completed else OPEN_TRUE["goals"],
    }

def multi_user_prefix(users: list[str], lifecycle: str) -> list[dict[str, object]]:
    events = [event("server_start")]
    events.extend(event("user_connect", user=user) for user in users)
    if lifecycle == "reconnect":
        events.extend(event("user_disconnect", user=user) for user in users)
        events.extend(event("user_connect", user=user) for user in users)
    elif lifecycle == "restart":
        events.append(event("server_kill"))
        events.append(event("server_start"))
        events.extend(event("user_connect", user=user) for user in users)
    return events

def user_setup(
    events: list[dict[str, object]],
    user: str,
    project_path: str,
    catalog: object,
    target: str,
    state: object,
) -> None:
    events.append(user_command(user, "start", {"project_path": project_path}, catalog))
    events.append(user_command(user, "prove", {"theorem": target}, state))

def close_success(user: str, state: dict[str, object], tactic: str) -> dict[str, object]:
    return user_command(
        user,
        "check",
        {"commands": tactic},
        {"state": {**state, "status": "Completed", "goals": ""}, "error": None},
    )

def close_lost(user: str, tactic: str) -> dict[str, object]:
    return user_command(
        user,
        "check",
        {"commands": tactic},
        {"kind": "not_found", "message": "proof attempt is no longer open"},
    )

def finish_users(events: list[dict[str, object]], users: list[str]) -> None:
    events.extend(event("user_disconnect", user=user) for user in users)
    events.append(event("server_kill"))

def write_dedicated_family(
    family: str,
    records: list[dict[str, object]],
    build: object,
) -> None:
    directory = ROOT / f"traces/{FIXTURE[family]}/generated/{family}"
    if directory.exists():
        shutil.rmtree(directory)
    for record in records:
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        events = build(record)
        target.write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )
        record["implementation"] = "implemented"

def materialize_two_users(records: list[dict[str, object]]) -> None:
    """Write two-user arbitration and independent-publication scenarios."""

    def build(record: dict[str, object]) -> list[dict[str, object]]:
        axes = record["axes"]
        assert isinstance(axes, dict)
        variant = str(axes["variant"])
        events = multi_user_prefix(["alice", "bob"], str(axes["lifecycle"]))
        if variant.startswith("same_theorem"):
            for user in ("alice", "bob"):
                user_setup(events, user, "matrix_project", PROOF_CATALOG, "truth", PROOF_OPEN)
            winner = "alice" if variant.endswith("alice_wins") else "bob"
            loser = "bob" if winner == "alice" else "alice"
            events.append(close_success(winner, PROOF_OPEN, "exact I."))
            events.append(close_lost(loser, "exact I."))
        elif variant == "different_theorem_same_project":
            user_setup(events, "alice", "matrix_project", PROOF_CATALOG, "truth", PROOF_OPEN)
            user_setup(events, "bob", "matrix_project", PROOF_CATALOG, "pair", PAIR_OPEN)
            events.append(close_success("alice", PROOF_OPEN, "exact I."))
            events.append(close_success("bob", PAIR_OPEN, "exact (conj I I)."))
        elif variant == "different_file_same_project":
            a = truth_state("Files.A.a", "Theorem a : True")
            b = truth_state("Files.B.b", "Theorem b : True")
            user_setup(events, "alice", "user_files_project", FILES_CATALOG, "a", a)
            user_setup(events, "bob", "user_files_project", FILES_CATALOG, "b", b)
            events.append(close_success("alice", a, "exact I."))
            events.append(close_success("bob", b, "exact I."))
        else:
            a = truth_state("ProjectA.Main.truth_a", "Theorem truth_a : True")
            b = truth_state("ProjectB.Main.truth_b", "Theorem truth_b : True")
            user_setup(events, "alice", "user_project_a", single_user_catalog("a"), "truth_a", a)
            user_setup(events, "bob", "user_project_b", single_user_catalog("b"), "truth_b", b)
            events.append(close_success("alice", a, "exact I."))
            events.append(close_success("bob", b, "exact I."))
        finish_users(events, ["alice", "bob"])
        return events

    write_dedicated_family("two_users", records, build)

def materialize_three_users(records: list[dict[str, object]]) -> None:
    """Write three-user winner and theorem/file/project relation scenarios."""

    users = ["alice", "bob", "carol"]

    def build(record: dict[str, object]) -> list[dict[str, object]]:
        axes = record["axes"]
        assert isinstance(axes, dict)
        variant = str(axes["variant"])
        events = multi_user_prefix(users, str(axes["lifecycle"]))
        states: dict[str, dict[str, object]] = {}
        tactics: dict[str, str] = {}
        targets: dict[str, str] = {}
        if variant.startswith("all_same"):
            for user in users:
                states[user] = PROOF_OPEN
                tactics[user] = "exact I."
                targets[user] = "truth"
                user_setup(events, user, "matrix_project", PROOF_CATALOG, "truth", PROOF_OPEN)
            winner = variant.removeprefix("all_same_").removesuffix("_wins")
            events.append(close_success(winner, states[winner], tactics[winner]))
            for user in users:
                if user != winner:
                    events.append(close_lost(user, tactics[user]))
        elif variant == "all_different_same_file":
            entries = {
                "alice": ("truth", PROOF_OPEN, "exact I."),
                "bob": ("pair", PAIR_OPEN, "exact (conj I I)."),
                "carol": (
                    "refl_nat",
                    {
                        "theorem": "Matrix.Main.refl_nat",
                        "statement": "Theorem refl_nat : forall n : nat, n = n",
                        "status": "Open",
                        "goals": (
                            "focused:\n  ============================\n"
                            "  forall n : nat, n = n\n"
                            "unfocused:\nshelved:\ngiven_up:\n"
                        ),
                    },
                    "intros n. reflexivity.",
                ),
            }
            for user, (target, state, tactic) in entries.items():
                user_setup(events, user, "matrix_project", PROOF_CATALOG, target, state)
                states[user], tactics[user] = state, tactic
            events.extend(close_success(user, states[user], tactics[user]) for user in users)
        elif variant == "different_files_same_project":
            for user, letter in zip(users, ("a", "b", "c"), strict=True):
                state = truth_state(f"Files.{letter.upper()}.{letter}", f"Theorem {letter} : True")
                user_setup(events, user, "user_files_project", FILES_CATALOG, letter, state)
                states[user], tactics[user] = state, "exact I."
            events.extend(close_success(user, states[user], tactics[user]) for user in users)
        elif variant == "different_projects":
            for user, letter in zip(users, ("a", "b", "c"), strict=True):
                state = truth_state(
                    f"Project{letter.upper()}.Main.truth_{letter}",
                    f"Theorem truth_{letter} : True",
                )
                user_setup(
                    events,
                    user,
                    f"user_project_{letter}",
                    single_user_catalog(letter),
                    f"truth_{letter}",
                    state,
                )
                states[user], tactics[user] = state, "exact I."
            events.extend(close_success(user, states[user], tactics[user]) for user in users)
        else:
            pair = variant.split("_same_")[0].split("_")
            winner = variant.split("_same_")[1].removesuffix("_wins")
            different = next(user for user in users if user not in pair)
            for user in pair:
                user_setup(events, user, "matrix_project", PROOF_CATALOG, "truth", PROOF_OPEN)
            user_setup(events, different, "matrix_project", PROOF_CATALOG, "pair", PAIR_OPEN)
            events.append(close_success(winner, PROOF_OPEN, "exact I."))
            events.append(close_success(different, PAIR_OPEN, "exact (conj I I)."))
            loser = next(user for user in pair if user != winner)
            events.append(close_lost(loser, "exact I."))
        finish_users(events, users)
        return events

    write_dedicated_family("three_users", records, build)
