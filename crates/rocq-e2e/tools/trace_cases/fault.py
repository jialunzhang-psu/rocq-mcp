"""Replayable publication crash-window cases via external MCP processes."""

from __future__ import annotations

import json
import shutil

from .common import OPEN_TRUE, command, event
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
        seed = {"name": "Demo.Main.seed", "statement": "Theorem seed : True", "status": "Completed"}
        recovered = {"name": theorem, "statement": "Theorem truth : True", "status": "Completed"}
        initial_catalog = {"declarations": [seed]}
        # Before metadata replacement, discovery sees only Main and appends
        # the durable Fresh record. Once both source and Dune metadata are
        # replaced, path-order discovery finds Fresh before Main.
        metadata_replaced = axes["point"] in {"after_replace", "after_final_build", "before_ack"}
        recovered_order = [recovered, seed] if multi and metadata_replaced else [seed, recovered]
        recovered_catalog = {"declarations": recovered_order}
        declared = {**OPEN_TRUE, "theorem": theorem, "statement": "Theorem truth : True"}
        completed = {**declared, "status": "Completed", "goals": ""}
        args = {"name": "truth", "statement": "True"}
        if multi:
            args["library"] = "Demo.Fresh"
        events = [
            event("server_start"),
            event("user_connect", user="alice"),
            command("start", {"project_path": project}, initial_catalog),
            command("declare", args, declared),
            command("check", {"commands": "exact I."}, {"$transport": "lost"}),
            event("server_kill"),
            event("server_start"),
            event("user_connect", user="alice"),
            command("start", {"project_path": project}, recovered_catalog),
        ]
        recovery = axes["recovery"]
        if recovery == "reconnect_retry":
            events.extend([
                event("user_disconnect", user="alice"),
                event("user_connect", user="alice"),
                command("start", {"project_path": project}, recovered_catalog),
            ])
        elif recovery == "restart_retry":
            events.extend([
                event("server_kill"),
                event("server_start"),
                event("user_connect", user="alice"),
                command("start", {"project_path": project}, recovered_catalog),
            ])
        events.extend([
            command("prove", {"theorem": "truth"}, completed),
            event("user_disconnect", user="alice"),
            event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
        record["implementation"] = "implemented"
