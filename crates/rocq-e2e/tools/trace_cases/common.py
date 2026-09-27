"""Shared trace events, oracles, and shard serialization."""

from __future__ import annotations

import json
import shutil
import subprocess
from copy import deepcopy
from collections.abc import Iterable
from pathlib import Path

from .matrix import FIXTURE, ROOT


ATTACHED = {"attached": True}

OPEN_TRUE = {
    "theorem": "Demo.Main.open_true",
    "statement": "Theorem open_true : True",
    "status": "Open",
    "goals": (
        "focused:\n  ============================\n  True\n"
        "unfocused:\nshelved:\ngiven_up:\n"
    ),
}

DONE_TRUE = {
    "theorem": "Demo.Main.done_true",
    "statement": "Theorem done_true : True",
    "status": "Completed",
    "goals": "",
}

PROOF_OPEN = {
    "theorem": "Matrix.Main.truth",
    "statement": "Theorem truth : True",
    "status": "Open",
    "goals": OPEN_TRUE["goals"],
}

PROOF_DONE = {
    "theorem": "Matrix.Main.done_true",
    "statement": "Theorem done_true : True",
    "status": "Completed",
    "goals": "",
}

def proof_state(state: dict[str, object], _layout: str) -> dict[str, object]:
    return dict(state)


def _is_state(value: object) -> bool:
    return (
        isinstance(value, dict)
        and {"theorem", "statement", "status", "goals"}.issubset(value)
    )


def _annotate_state_checkpoints(events: list[dict[str, object]]) -> None:
    """Mark states that must expose a selected request checkpoint.

    Checkpoint values are connection-local runtime IDs, so generated traces
    assert their positive-integer type rather than duplicating the server's ID
    allocator. Focused stdio tests check monotonicity and exact rewind targets.
    """
    for event_record in events:
        if event_record.get("event") != "command":
            continue
        command = event_record.get("command")
        if not isinstance(command, dict):
            continue
        tool = command.get("tool")
        args = command.get("args")
        args = args if isinstance(args, dict) else {}
        expected = event_record.get("expected")
        if not isinstance(expected, dict):
            continue
        rewritten = deepcopy(expected)
        state: object | None = None
        if tool in {"prove", "declare"}:
            state = rewritten
        elif tool in {"check", "rewind"}:
            state = rewritten.get("state")
        elif tool == "query" and args.get("kind") == "goals":
            state = rewritten
        if _is_state(state) and state.get("status") == "Open":
            state["checkpoint"] = {"$checkpoint": True}
            event_record["expected"] = rewritten


def declaration(qualified_path: str, file: str = "Main.v") -> dict[str, object]:
    """Build the wire identity returned by ``list_decls(file)``.

    ``qualified_path`` is the complete PET path; ``file`` is already relative
    to the fixture's Dune workspace.  This helper performs no source discovery
    and deliberately has no fallback path.
    """
    return {"file": file, "qualified_path": qualified_path.split(".")}


def prove_args(qualified_path: str, file: str = "Main.v") -> dict[str, object]:
    """Return current-protocol arguments for selecting one declaration."""
    return {"declaration": declaration(qualified_path, file)}

def event(name: str, **fields: object) -> dict[str, object]:
    return {"event": name, **fields}

def command(tool: str, args: dict[str, object], expected: object) -> dict[str, object]:
    return event(
        "command",
        user="alice",
        command={"tool": tool, "args": args},
        expected=expected,
    )

def user_command(
    user: str, tool: str, args: dict[str, object], expected: object
) -> dict[str, object]:
    return event(
        "command",
        user=user,
        command={"tool": tool, "args": args},
        expected=expected,
    )

def selection_setup(selection: str) -> list[dict[str, object]]:
    if selection == "no_project":
        return []
    setup = [command("start", {"project_path": "project"}, ATTACHED)]
    if selection == "open":
        setup.append(command("prove", prove_args("Demo.Main.open_true"), OPEN_TRUE))
    elif selection == "completed":
        setup.append(command("prove", prove_args("Demo.Main.done_true"), DONE_TRUE))
    return setup

def lifecycle_prefix(selection: str, lifecycle: str) -> list[dict[str, object]]:
    prefix = [event("server_start"), event("user_connect", user="alice")]
    prefix.extend(selection_setup(selection))
    if lifecycle == "reconnect":
        prefix.extend(
            [event("user_disconnect", user="alice"), event("user_connect", user="alice")]
        )
        prefix.extend(selection_setup(selection))
    elif lifecycle == "restart":
        prefix.extend(
            [
                event("server_kill"),
                event("server_start"),
                event("user_connect", user="alice"),
            ]
        )
        prefix.extend(selection_setup(selection))
    return prefix

def proof_selection_setup(selection: str, layout: str) -> list[dict[str, object]]:
    if selection == "no_project":
        return []
    project_path = "matrix_dune_project"
    setup = [command("start", {"project_path": project_path}, ATTACHED)]
    if selection == "open":
        setup.append(
            command(
                "prove",
                prove_args("Matrix.Main.truth", "theories/Main.v"),
                proof_state(PROOF_OPEN, layout),
            )
        )
    elif selection == "completed":
        setup.append(
            command(
                "prove",
                prove_args("Matrix.Main.done_true", "theories/Main.v"),
                proof_state(PROOF_DONE, layout),
            )
        )
    return setup

def proof_lifecycle_prefix(
    selection: str, lifecycle: str, layout: str
) -> list[dict[str, object]]:
    """Build a proof-fixture lifecycle without relying on mutable publication."""
    prefix = [event("server_start"), event("user_connect", user="alice")]
    prefix.extend(proof_selection_setup(selection, layout))
    if lifecycle == "reconnect":
        prefix.extend(
            [event("user_disconnect", user="alice"), event("user_connect", user="alice")]
        )
        prefix.extend(proof_selection_setup(selection, layout))
    elif lifecycle == "restart":
        prefix.extend(
            [
                event("server_kill"),
                event("server_start"),
                event("user_connect", user="alice"),
            ]
        )
        prefix.extend(proof_selection_setup(selection, layout))
    return prefix

def invalid_request(message: str) -> dict[str, str]:
    return {"kind": "invalid_request", "message": message}

def write_jsonl(target: Path, records: Iterable[dict[str, object]], *, sort_keys: bool = False) -> None:
    """Stream JSONL records to plain or zstd storage without buffering the corpus."""
    if isinstance(records, list) and any(
        isinstance(record, dict) and record.get("event") == "command"
        for record in records
    ):
        _annotate_state_checkpoints(records)
    if target.suffix == ".zst":
        # Design note: boundary cases contain megabyte-scale inputs. Stream
        # directly into zstd rather than constructing a gigabyte string.
        with subprocess.Popen(
            ["zstd", "-1", "-q", "-f", "-o", str(target)],
            stdin=subprocess.PIPE,
            text=True,
        ) as compressor:
            assert compressor.stdin is not None
            for raw in records:
                compressor.stdin.write(
                    json.dumps(raw, ensure_ascii=False, sort_keys=sort_keys, separators=(",", ":")) + "\n"
                )
            compressor.stdin.close()
            if compressor.wait() != 0:
                raise RuntimeError(f"zstd failed while writing {target}")
    else:
        with target.open("w") as output:
            for raw in records:
                output.write(
                    json.dumps(raw, ensure_ascii=False, sort_keys=sort_keys, separators=(",", ":")) + "\n"
                )

def write_materialized_family(
    family: str,
    records: list[dict[str, object]],
    append_cases: object,
) -> None:
    """Write every assigned shard for one ordinary command family.

    ``append_cases`` receives ``(events, members)`` after the lifecycle setup
    and must append one command assertion per member. This function owns the
    generated family directory and marks a case implemented only after its
    trace has been serialized.
    """
    directory = ROOT / f"traces/{FIXTURE[family]}/generated/{family}"
    # Design note: generated families have exactly one owner. Removing the
    # directory prevents an obsolete shard from remaining executable after a
    # matrix or grouping change.
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    grouped: dict[str, list[dict[str, object]]] = {}
    for record in records:
        grouped.setdefault(str(record["trace"]), []).append(record)
    for relative, members in grouped.items():
        members.sort(key=lambda record: int(record["case_index"]))
        axes = members[0]["axes"]
        assert isinstance(axes, dict)
        if family == "try_failures":
            default_selection = "open"
        elif family in {
            "declare_boundaries",
            "declare_failures",
            "prove_failures",
            "query_failures",
            "check_publication",
        }:
            default_selection = "project"
        else:
            default_selection = "no_project"
        selection = str(axes.get("selection", default_selection))
        lifecycle = str(axes.get("lifecycle", "connected"))
        layout = str(axes.get("layout", "dune"))
        events = (
            proof_lifecycle_prefix(selection, lifecycle, layout)
            if FIXTURE[family] == "proof"
            else lifecycle_prefix(selection, lifecycle)
        )
        append_cases(events, members)
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        target = ROOT / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(target, events)
        for record in members:
            record["implementation"] = "implemented"
