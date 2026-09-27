"""Replayable publication crash-window cases via external MCP processes."""

from __future__ import annotations

import shutil

from .common import ATTACHED, OPEN_TRUE, command, event, prove_args, write_jsonl
from .matrix import ROOT


def materialize_publication_fault(records: list[dict[str, object]]) -> None:
    """Write every fault-point × replacement-shape × recovery crossing.

    The fixture wrapper arms the named point only in the first server process.
    A fault-injection build aborts that process after the candidate is durable;
    all recovery commands still use only public MCP on a new external process.
    """
    directory = ROOT / "traces/fault/generated/publication_fault"
    if directory.exists():
        shutil.rmtree(directory)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        multi = axes["replacement"] == "multi_file"
        project = "project_multi" if multi else "project_single"
        theorem = "Demo.Fresh.truth" if multi else "Demo.Main.truth"
        # Before metadata replacement, discovery sees only Main and appends
        # the durable Fresh record. Once both source and Dune metadata are
        # replaced, path-order discovery finds Fresh before Main.
        declared = {**OPEN_TRUE, "theorem": theorem, "statement": "Theorem truth : True"}
        completed = {**declared, "status": "Completed", "goals": ""}
        args = {"name": "truth", "statement": "True", "kind": "Theorem", "library": "Demo.Fresh" if multi else "Demo.Main", "file": "theories/Fresh.v" if multi else "Main.v"}
        events = [
            event("server_start"),
            event("user_connect", user="alice"),
            command("start", {"project_path": project}, ATTACHED),
            command("declare", args, declared),
            command("check", {"attempts": ["exact I."]}, {"$transport": "lost"}),
            event("server_kill"),
            event("server_start"),
            event("user_connect", user="alice"),
            command("start", {"project_path": project}, ATTACHED),
        ]
        recovery = axes["recovery"]
        if recovery == "reconnect_retry":
            events.extend([
                event("user_disconnect", user="alice"),
                event("user_connect", user="alice"),
                command("start", {"project_path": project}, ATTACHED),
            ])
        elif recovery == "restart_retry":
            events.extend([
                event("server_kill"),
                event("server_start"),
                event("user_connect", user="alice"),
                command("start", {"project_path": project}, ATTACHED),
            ])
        events.extend([
            command("prove", prove_args(theorem, "theories/Fresh.v" if multi else "Main.v"), completed),
            event("user_disconnect", user="alice"),
            event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(target, events)
        record["implementation"] = "implemented"
