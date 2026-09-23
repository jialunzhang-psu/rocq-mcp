"""Shared trace events, oracles, and shard serialization."""

from __future__ import annotations

import json
import shutil
import subprocess
from collections.abc import Iterable
from pathlib import Path

from .matrix import FIXTURE, ROOT


BASIC_CATALOG = {
    "declarations": [
        {
            "name": "Demo.Main.open_true",
            "statement": "Theorem open_true : True",
            "status": "Open",
        },
        {
            "name": "Demo.Main.done_true",
            "statement": "Theorem done_true : True",
            "status": "Completed",
        },
        {
            "name": "Demo.Main.answer",
            "statement": "Definition answer : nat",
            "status": "Completed",
        },
        {
            "name": "Demo.Main.duplicate",
            "statement": "Theorem duplicate : True",
            "status": "Open",
        },
        {
            "name": "Demo.Main.Nested.duplicate",
            "statement": "Theorem duplicate : True",
            "status": "Open",
        },
    ]
}

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

DUNE_CATALOG = {
    "declarations": [
        {
            "name": "DuneDemo.Main.truth",
            "statement": "Theorem truth : True",
            "status": "Open",
        }
    ]
}

SINGLE_CATALOG = {
    "declarations": [
        {
            "name": "Single.Main.truth",
            "statement": "Theorem truth : True",
            "status": "Open",
        }
    ]
}

PROOF_CATALOG = {
    "declarations": [
        {
            "name": "Matrix.Main.truth",
            "statement": "Theorem truth : True",
            "status": "Open",
        },
        {
            "name": "Matrix.Main.pair",
            "statement": "Theorem pair : True /\\ True",
            "status": "Open",
        },
        {
            "name": "Matrix.Main.refl_nat",
            "statement": "Theorem refl_nat : forall n : nat, n = n",
            "status": "Open",
        },
        {
            "name": "Matrix.Main.done_true",
            "statement": "Theorem done_true : True",
            "status": "Completed",
        },
        {
            "name": "Matrix.Main.number",
            "statement": "Definition number : nat",
            "status": "Open",
        },
    ]
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

def proof_catalog(layout: str) -> dict[str, object]:
    if layout == "coqproject":
        return PROOF_CATALOG
    # Keep one semantic catalog and change only the logical library prefix.
    return {
        "declarations": [
            {**item, "name": str(item["name"]).replace("Matrix.", "MatrixDune.", 1)}
            for item in PROOF_CATALOG["declarations"]
        ]
    }

def proof_state(state: dict[str, object], layout: str) -> dict[str, object]:
    if layout == "coqproject":
        return state
    return {
        **state,
        "theorem": str(state["theorem"]).replace("Matrix.", "MatrixDune.", 1),
    }

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
    setup = [command("start", {"project_path": "project"}, BASIC_CATALOG)]
    if selection == "open":
        setup.append(command("prove", {"theorem": "open_true"}, OPEN_TRUE))
    elif selection == "completed":
        setup.append(command("prove", {"theorem": "done_true"}, DONE_TRUE))
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
    project_path = "matrix_project" if layout == "coqproject" else "matrix_dune_project"
    setup = [command("start", {"project_path": project_path}, proof_catalog(layout))]
    if selection == "open":
        setup.append(command("prove", {"theorem": "truth"}, proof_state(PROOF_OPEN, layout)))
    elif selection == "completed":
        setup.append(
            command("prove", {"theorem": "done_true"}, proof_state(PROOF_DONE, layout))
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
    if target.suffix == ".zst":
        # Design note: boundary cases contain megabyte-scale inputs. Stream
        # directly into zstd rather than constructing a gigabyte string.
        with subprocess.Popen(
            ["zstd", "-1", "-q", "-f", "-o", str(target)],
            stdin=subprocess.PIPE,
            text=True,
        ) as compressor:
            assert compressor.stdin is not None
            for item in records:
                compressor.stdin.write(
                    json.dumps(item, ensure_ascii=False, sort_keys=sort_keys, separators=(",", ":")) + "\n"
                )
            compressor.stdin.close()
            if compressor.wait() != 0:
                raise RuntimeError(f"zstd failed while writing {target}")
    else:
        with target.open("w") as output:
            for item in records:
                output.write(
                    json.dumps(item, ensure_ascii=False, sort_keys=sort_keys, separators=(",", ":")) + "\n"
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
        if family == "check_multi_failures":
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
        layout = str(axes.get("layout", "coqproject"))
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
