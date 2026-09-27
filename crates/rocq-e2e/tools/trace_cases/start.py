"""Project start and external-environment scenarios."""

from __future__ import annotations

import shutil

from .common import (
    ATTACHED,
    OPEN_TRUE,
    command,
    event,
    invalid_request,
    prove_args,
    write_jsonl,
    write_materialized_family,
)
from .matrix import ROOT


def start_parameters_args(project_path: str, extra: str, _layout: str) -> dict[str, object]:
    args: dict[str, object] = {}
    if project_path == "null":
        args["project_path"] = None
    elif project_path == "wrong_type":
        args["project_path"] = 0
    elif project_path == "empty":
        args["project_path"] = ""
    elif project_path == "valid":
        args["project_path"] = "dune_project"
    if extra == "present":
        args["extra"] = True
    return args

def start_parameters_expected(
    project_path: str, extra: str, _layout: str
) -> object:
    if extra == "present":
        return invalid_request("unexpected field 'extra'")
    if project_path != "valid":
        return invalid_request("project_path must be a non-empty string")
    return ATTACHED

def materialize_start_parameters(records: list[dict[str, object]]) -> None:
    """Write exhaustive start argument-shape traces."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            project_path = str(axes["project_path"])
            extra = str(axes["extra"])
            layout = str(axes["layout"])
            events.append(
                command(
                    "start",
                    start_parameters_args(project_path, extra, layout),
                    start_parameters_expected(project_path, extra, layout),
                )
            )

    write_materialized_family("start_parameters", records, append)

def start_path(catalog: str, path: str) -> str:
    base = {
        "empty": "empty_project",
        "single": "single_project",
        "mixed_status": "project",
    }[catalog]
    if path == "relative":
        return base
    if path == "absolute":
        # `/proc/self/cwd` is resolved by the external MCP child, whose working
        # directory is the disposable fixture root.
        return f"/proc/self/cwd/{base}"
    if path == "spaces":
        return f"{base} space"
    if path == "unicode":
        return f"{base}_项目"
    if path == "dot_segments":
        return f"./{base}/../{base}"
    if path == "missing":
        return "missing-project"
    if path == "regular_file":
        return "project/Main.v"
    if path == "empty_directory":
        return "empty_project"
    if path == "malformed_dune":
        return "malformed_dune"
    raise AssertionError(path)

def start_path_expected(catalog: str, path: str) -> object:
    if path == "missing":
        return {"kind": "invalid_configuration", "message": "project path is unavailable"}
    if path == "regular_file":
        return {
            "kind": "invalid_configuration",
            "message": "Dune is unavailable",
        }
    if path == "malformed_dune":
        return {
            "kind": "invalid_configuration",
            "message": "Dune workspace discovery failed",
        }
    return ATTACHED

def materialize_start_paths(records: list[dict[str, object]]) -> None:
    """Write path spelling, layout failure and catalog-cardinality cases."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            catalog = str(axes["catalog"])
            path = str(axes["path"])
            events.append(
                command(
                    "start",
                    {"project_path": start_path(catalog, path)},
                    start_path_expected(catalog, path),
                )
            )

    write_materialized_family("start_paths", records, append)

def environment_change_is_materialized(axes: dict[str, object]) -> bool:
    """Identify environment transitions with a checked exact public oracle."""
    return axes["change"] in {
        "source_same_mtime", "source_added", "source_deleted",
        "source_restored", "dune_modules", "configuration_changed",
    }

def materialize_environment_change(records: list[dict[str, object]]) -> None:
    """Replay source mutations through fixture-controlled lifecycle boundaries.

    The wrapper or after-event fixture hook changes only disposable source
    files. The first proof is deliberately open; its next public observation
    checks the resulting source, load-path, configuration, or VO view.
    """
    selected = [
        record for record in records
        if isinstance(record["axes"], dict)
        and environment_change_is_materialized(record["axes"])
    ]
    directory = ROOT / "traces/source_change/generated/environment_change"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in selected:
        axes = record["axes"]
        assert isinstance(axes, dict)
        change = str(axes["change"])
        recovery = str(axes["recovery"])
        projects = {
            "source_same_mtime": "project_content",
            "source_added": "project_add",
            "source_deleted": "project_delete",
            "source_restored": "project_restore",
            "dune_modules": "project_dune",
            "configuration_changed": "project_config_dune",
        }
        project = projects[change]
        declaration_file = (
            "theories/Main.v"
            if change in {"dune_modules", "configuration_changed"}
            else "Main.v"
        )
        library = "Demo.Main"
        changed = (
            {"kind": "invalid_configuration", "message": "Dune workspace discovery failed"}
            if change == "configuration_changed"
            else ATTACHED
        )
        events = [
            event("server_start"),
            event("user_connect", user="alice"),
            command("start", {"project_path": project}, ATTACHED),
            command("prove", prove_args("Demo.Main.truth", declaration_file), {
                "theorem": f"{library}.truth", "statement": "Theorem truth : True",
                "status": "Open", "goals": OPEN_TRUE["goals"],
            }),
        ]
        if recovery == "restart":
            events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
        elif recovery == "reconnect":
            events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
        events.append(command("start", {"project_path": project}, changed))
        if change == "source_restored":
            if recovery == "restart":
                events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
            events.append(command("start", {"project_path": project}, ATTACHED))
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(target, events)
        record["implementation"] = "implemented"

def start_failure_is_materialized(axes: dict[str, object]) -> bool:
    return axes["failure"] in {"project_unavailable", "layout_invalid"}

def materialize_start_failures(records: list[dict[str, object]]) -> None:
    """Write each stable failure and its post-disconnect source mutation crossing."""
    selected = []
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if start_failure_is_materialized(axes):
            selected.append(record)

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            failure = str(axes["failure"])
            if failure == "project_unavailable":
                path = "missing-project"
                expected = {
                    "kind": "invalid_configuration",
                    "message": "project path is unavailable",
                }
            elif failure == "layout_invalid":
                path = "malformed_dune"
                expected = {
                    "kind": "invalid_configuration",
                    "message": "Dune workspace discovery failed",
                }
            else:
                raise AssertionError(failure)
            if axes["source"] == "changed_while_disconnected":
                events.extend([
                    command("start", {"project_path": "project"}, ATTACHED),
                    event("user_disconnect", user="alice"),
                    event("user_connect", user="alice"),
                    command("start", {"project_path": "project"}, expected),
                ])
            else:
                events.append(command("start", {"project_path": path}, expected))

    write_materialized_family("start_failures", selected, append)
