"""Proof and target-query scenarios."""

from __future__ import annotations

import shutil

from .common import (
    ATTACHED,
    DONE_TRUE,
    OPEN_TRUE,
    command,
    declaration,
    event,
    invalid_request,
    prove_args,
    user_command,
    write_jsonl,
    write_materialized_family,
)
from .matrix import ROOT
from .multiuser import close_success


def prove_selection_setup(selection: str) -> list[dict[str, object]]:
    if selection == "no_project":
        return []
    setup = [command("start", {"project_path": "prove_project"}, ATTACHED)]
    if selection == "open":
        setup.append(command("prove", prove_args("Demo.Main.open_true"), OPEN_TRUE))
    elif selection == "completed":
        setup.append(command("prove", prove_args("Demo.Main.done_true"), DONE_TRUE))
    return setup

def prove_lifecycle_prefix(selection: str, lifecycle: str) -> list[dict[str, object]]:
    events = [event("server_start"), event("user_connect", user="alice")]
    events.extend(prove_selection_setup(selection))
    if lifecycle == "reconnect":
        events.extend(
            [event("user_disconnect", user="alice"), event("user_connect", user="alice")]
        )
        events.extend(prove_selection_setup(selection))
    elif lifecycle == "restart":
        events.extend(
            [event("server_kill"), event("server_start"), event("user_connect", user="alice")]
        )
        events.extend(prove_selection_setup(selection))
    return events

def prove_target_args(target: str, extra: str) -> dict[str, object]:
    args: dict[str, object] = {}
    if target == "null":
        args["declaration"] = None
    elif target == "wrong_type":
        args["declaration"] = "Demo.Main.open_true"
    elif target == "empty":
        args["declaration"] = {}
    elif target != "missing_field":
        qualified = {
            "full_name": "Demo.Main.open_true",
            "completed": "Demo.Main.done_true",
            "not_found": "missing",
            "invalid_syntax": "bad-name",
            "at_limit": "a" * (64 * 1024),
            "over_limit": "a" * (64 * 1024 + 1),
        }[target]
        args["declaration"] = declaration(qualified)
    if extra == "present":
        args["extra"] = True
    return args

def prove_target_expected(target: str, extra: str, selection: str) -> object:
    if extra == "present":
        return invalid_request("unexpected field 'extra'")
    if selection == "no_project":
        return invalid_request("call start first")
    if target == "missing_field":
        return invalid_request("declaration is required")
    if target in {"null", "wrong_type"}:
        return invalid_request("declaration must be an object")
    if target == "empty":
        return invalid_request("declaration.file is required")
    if target == "full_name":
        return OPEN_TRUE
    if target == "completed":
        return DONE_TRUE
    if target in {"invalid_syntax", "over_limit"}:
        return {
            "kind": "invalid_declaration",
            "message": "declaration identity is invalid",
        }
    requested = "missing" if target == "not_found" else "a" * (64 * 1024)
    return {
        "kind": "not_found",
        "message": f"declaration '{requested}' was not found",
    }

def materialize_prove(records: list[dict[str, object]]) -> None:
    """Write deterministic and isolated-runtime prove cases."""
    directory = ROOT / "traces/proof/generated/prove"
    if directory.exists():
        shutil.rmtree(directory)
    selected = [
        record
        for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["runtime"] == "alive"
    ]
    grouped: dict[str, list[dict[str, object]]] = {}
    for record in selected:
        grouped.setdefault(str(record["trace"]), []).append(record)
    for relative, members in grouped.items():
        members.sort(key=lambda record: int(record["case_index"]))
        axes = members[0]["axes"]
        assert isinstance(axes, dict)
        selection = str(axes["selection"])
        events = prove_lifecycle_prefix(selection, str(axes["lifecycle"]))
        for record in members:
            case_axes = record["axes"]
            assert isinstance(case_axes, dict)
            target = str(case_axes["target"])
            extra = str(case_axes["extra"])
            events.append(
                command(
                    "prove",
                    prove_target_args(target, extra),
                    prove_target_expected(target, extra, selection),
                )
            )
            record["implementation"] = "implemented"
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        path = ROOT / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(path, events)
    materialize_prove_pet_killed(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["runtime"] == "pet_killed"
        ]
    )
    materialize_prove_pet_evicted(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["runtime"] == "pet_evicted"
        ]
    )
    materialize_prove_timeouts(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["runtime"] == "proof_timeout"
        ]
    )

def append_pet_detacher(
    events: list[dict[str, object]], record: dict[str, object]
) -> None:
    """Publish an independent theorem so the next replay must spawn a PET."""
    events.append(event("user_connect", user="bob"))
    events.append(
        user_command(
            "bob", "start", {"project_path": "prove_project"}, ATTACHED
        )
    )
    name = f"detacher_{record['case']}"
    state = {
        "theorem": f"Demo.Main.{name}",
        "statement": f"Theorem {name} : True",
        "status": "Open",
        "goals": OPEN_TRUE["goals"],
    }
    events.append(
        user_command(
            "bob",
            "declare",
            {
                "name": name,
                "statement": "True",
                "library": "Demo.Main",
                "file": "Main.v",
            },
            state,
        )
    )
    events.append(close_success("bob", state, "exact I."))

def materialize_prove_pet_killed(records: list[dict[str, object]]) -> None:
    """Exercise every prove case with a real PET death/retry lane."""
    directory = ROOT / "traces/pet_fault/generated/prove_pet_killed"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        selection = str(axes["selection"])
        events = prove_lifecycle_prefix(selection, str(axes["lifecycle"]))
        bob_connected = False
        if selection == "open":
            # The selected proof has already exercised one death/retry pair.
            # Publishing Bob's independent declaration detaches that live PET,
            # so Alice's actual prove command must spawn, die, and replay again.
            bob_connected = True
            append_pet_detacher(events, record)
        target = str(axes["target"])
        extra = str(axes["extra"])
        events.append(
            command(
                "prove",
                prove_target_args(target, extra),
                prove_target_expected(target, extra, selection),
            )
        )
        if bob_connected:
            events.append(event("user_disconnect", user="bob"))
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        write_jsonl((ROOT / str(record["trace"])), events)
        record["implementation"] = "implemented"

def prove_timeout_expected(target: str, extra: str, selection: str) -> object:
    if extra == "present" or selection == "no_project" or target != "full_name":
        return prove_target_expected(target, extra, selection)
    return {"kind": "proof_timeout", "message": "PET operation timed out"}

def materialize_prove_timeouts(records: list[dict[str, object]]) -> None:
    """Hang the target PET spawn after establishing the requested selection."""
    directory = ROOT / "traces/pet_timeout/generated/prove_proof_timeout"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        selection = str(axes["selection"])
        events = prove_lifecycle_prefix(selection, str(axes["lifecycle"]))
        bob_connected = selection == "open"
        if bob_connected:
            append_pet_detacher(events, record)
        target = str(axes["target"])
        extra = str(axes["extra"])
        events.append(
            command(
                "prove",
                prove_target_args(target, extra),
                prove_timeout_expected(target, extra, selection),
            )
        )
        if bob_connected:
            events.append(event("user_disconnect", user="bob"))
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        target_path = ROOT / str(record["trace"])
        target_path.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(target_path, events)
        record["implementation"] = "implemented"

def materialize_prove_pet_evicted(records: list[dict[str, object]]) -> None:
    """Evict Alice's PET with a second project, then replay every prove case."""
    directory = ROOT / "traces/pet_eviction/generated/prove_pet_evicted"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    b_state = {
        "theorem": "EvictB.Main.truth_b",
        "statement": "Theorem truth_b : True",
        "status": "Open",
        "goals": OPEN_TRUE["goals"],
    }
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        lifecycle = str(axes["lifecycle"])
        selection = str(axes["selection"])
        events = [event("server_start"), event("user_connect", user="alice")]
        if lifecycle == "reconnect":
            events.extend(
                [
                    event("user_disconnect", user="alice"),
                    event("user_connect", user="alice"),
                ]
            )
        elif lifecycle == "restart":
            events.extend(
                [
                    event("server_kill"),
                    event("server_start"),
                    event("user_connect", user="alice"),
                ]
            )
        carol_connected = selection == "no_project"
        owner = "carol" if carol_connected else "alice"
        if carol_connected:
            events.append(event("user_connect", user="carol"))
        events.append(
            user_command(
                owner,
                "start",
                {"project_path": "prove_project"},
                ATTACHED,
            )
        )
        events.append(
            user_command(owner, "prove", prove_args("Demo.Main.open_true"), OPEN_TRUE)
        )
        if selection == "project":
            events.append(
                user_command(
                    "alice",
                    "start",
                    {"project_path": "prove_project"},
                    ATTACHED,
                )
            )
        elif selection == "completed":
            events.append(
                user_command("alice", "prove", prove_args("Demo.Main.done_true"), DONE_TRUE)
            )
        events.append(event("user_connect", user="bob"))
        events.append(
            user_command(
                "bob", "start", {"project_path": "other_project"}, ATTACHED
            )
        )
        events.append(
            user_command("bob", "prove", prove_args("EvictB.Main.truth_b"), b_state)
        )
        target = str(axes["target"])
        extra = str(axes["extra"])
        events.append(
            command(
                "prove",
                prove_target_args(target, extra),
                prove_target_expected(target, extra, selection),
            )
        )
        events.append(event("user_disconnect", user="bob"))
        if carol_connected:
            events.append(event("user_disconnect", user="carol"))
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        write_jsonl((ROOT / str(record["trace"])), events)
        record["implementation"] = "implemented"

def prove_failure_is_materialized(axes: dict[str, object]) -> bool:
    return axes["failure"] in {
        "not_found",
        "proof_timeout",
        "closed_attempt",
        "invalid_configuration",
    }

def materialize_prove_failures(records: list[dict[str, object]]) -> None:
    """Write target-resolution failures independent of external fault setup."""
    deterministic = []
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if axes["failure"] in {"not_found", "closed_attempt"}:
            deterministic.append(record)

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["failure"] == "not_found":
                expected = {
                    "kind": "not_found",
                    "message": "declaration 'missing' was not found",
                }
                target = "missing"
            else:
                events.append(command("prove", prove_args("Demo.Main.open_true"), OPEN_TRUE))
                events.append(close_success("alice", OPEN_TRUE, "exact I."))
                events.append(
                    command(
                        "prove",
                        prove_args("Demo.Main.open_true"),
                        {**OPEN_TRUE, "status": "Completed", "goals": ""},
                    )
                )
                continue
            events.append(command("prove", prove_args(target), expected))

    write_materialized_family("prove_failures", deterministic, append)
    materialize_prove_failure_timeouts(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["failure"] == "proof_timeout"
        ]
    )
    materialize_prove_configuration_failures([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["failure"] == "invalid_configuration"
    ])

def materialize_prove_configuration_failures(records: list[dict[str, object]]) -> None:
    """Prove must reject a selected project whose load path later breaks."""
    directory = ROOT / "traces/source_change/generated/prove_failures"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    failure = {"kind": "invalid_configuration", "message": "Dune workspace discovery failed"}
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        lifecycle = str(axes["lifecycle"])
        events = [event("server_start"), event("user_connect", user="alice")]
        if lifecycle != "connected":
            events.append(command("start", {"project_path": "project_config_dune"}, ATTACHED))
            if lifecycle == "reconnect":
                events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
            else:
                events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
        events.extend([
            command("start", {"project_path": "project_config_dune"}, ATTACHED),
            command(
                "prove",
                prove_args("Demo.Main.truth", "theories/Main.v"),
                failure,
            ),
            event("user_disconnect", user="alice"), event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(target, events)
        record["implementation"] = "implemented"

def materialize_prove_failure_timeouts(records: list[dict[str, object]]) -> None:
    """Exercise the prove failure taxonomy with an actual hanging PET."""
    directory = ROOT / "traces/pet_timeout/generated/prove_failures"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        events = prove_lifecycle_prefix("project", str(axes["lifecycle"]))
        events.append(
            command(
                "prove",
                prove_args("Demo.Main.open_true"),
                {"kind": "proof_timeout", "message": "PET operation timed out"},
            )
        )
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        write_jsonl((ROOT / str(record["trace"])), events)
        record["implementation"] = "implemented"
