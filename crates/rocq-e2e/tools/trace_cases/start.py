"""Project start and external-environment scenarios."""

from __future__ import annotations

import json
import shutil

from .matrix import ROOT
from .common import BASIC_CATALOG, DUNE_CATALOG, OPEN_TRUE, SINGLE_CATALOG, command, event, invalid_request, write_materialized_family


def start_parameters_args(project_path: str, extra: str, layout: str) -> dict[str, object]:
    args: dict[str, object] = {}
    if project_path == "null":
        args["project_path"] = None
    elif project_path == "wrong_type":
        args["project_path"] = 0
    elif project_path == "empty":
        args["project_path"] = ""
    elif project_path == "valid":
        args["project_path"] = "project" if layout == "coqproject" else "dune_project"
    if extra == "present":
        args["extra"] = True
    return args

def start_parameters_expected(
    project_path: str, extra: str, layout: str
) -> object:
    if extra == "present":
        return invalid_request("unexpected field 'extra'")
    if project_path != "valid":
        return invalid_request("project_path must be a non-empty string")
    return BASIC_CATALOG if layout == "coqproject" else DUNE_CATALOG

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
    if path == "ambiguous_layout":
        return "ambiguous_project"
    if path == "malformed_coqproject":
        return "malformed_coqproject"
    if path == "malformed_dune":
        return "malformed_dune"
    raise AssertionError(path)

def start_path_expected(catalog: str, path: str) -> object:
    if path == "missing":
        return {"kind": "invalid_configuration", "message": "project path is unavailable"}
    if path == "regular_file":
        return {
            "kind": "invalid_configuration",
            "message": "project state directory is unavailable",
        }
    if path == "ambiguous_layout":
        return {"kind": "ambiguous", "message": "project layout is ambiguous"}
    if path == "malformed_coqproject":
        return {
            "kind": "invalid_configuration",
            "message": "_CoqProject load path is incomplete",
        }
    if path == "malformed_dune":
        return {
            "kind": "invalid_configuration",
            "message": "unterminated project expression",
        }
    if path == "empty_directory" or catalog == "empty":
        return {"declarations": []}
    return SINGLE_CATALOG if catalog == "single" else BASIC_CATALOG

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
    if axes["candidate"] == "solved_pending":
        return axes["change"] in {"source_added", "source_same_mtime", "source_deleted", "source_restored", "dune_modules", "load_path", "axiom_after_baseline"} or (
            axes["change"] in {"direct_vo_changed", "direct_vo_deleted", "transitive_vo_changed", "plugin_vo_changed"}
        ) or (
            axes["change"] in {"rocq_identity_changed", "pet_identity_changed"}
        ) or (
            axes["change"] == "configuration_changed"
        )
    if axes["candidate"] != "open":
        return False
    if axes["change"] in {
        "source_same_mtime", "source_added", "source_deleted",
        "source_restored", "dune_modules", "load_path", "configuration_changed",
    }:
        return True
    if axes["change"] in {
        "direct_vo_changed", "direct_vo_deleted", "transitive_vo_changed",
        "plugin_vo_changed",
    }:
        return True
    return axes["change"] in {
        "rocq_identity_changed", "pet_identity_changed",
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
        if axes["candidate"] == "solved_pending":
            if change == "axiom_after_baseline":
                materialize_pending_axiom_change(record)
            elif change in {"direct_vo_changed", "direct_vo_deleted", "transitive_vo_changed", "plugin_vo_changed"}:
                materialize_pending_vo_change(record)
            elif change in {"rocq_identity_changed", "pet_identity_changed"}:
                materialize_toolchain_identity_change(record)
            elif change == "configuration_changed":
                materialize_pending_configuration_change(record)
            else:
                materialize_pending_environment_change(record)
            continue
        if change in {"rocq_identity_changed", "pet_identity_changed"}:
            materialize_toolchain_identity_change(record)
            continue
        if change in {"direct_vo_changed", "direct_vo_deleted", "transitive_vo_changed", "plugin_vo_changed"}:
            materialize_vo_change(record, change)
            continue
        projects = {
            "source_same_mtime": "project_content",
            "source_added": "project_add",
            "source_deleted": "project_delete",
            "source_restored": "project_restore",
            "dune_modules": "project_dune",
            "load_path": "project_loadpath",
            "configuration_changed": "project_config",
        }
        project = projects[change]
        library = "Demo.Main"
        initial = {"declarations": [
            {"name": f"{library}.truth", "statement": "Theorem truth : True", "status": "Open"}
        ]}
        changed = {"declarations": [
            {"name": f"{library}.truth", "statement": "Theorem truth : False" if change == "source_same_mtime" else "Theorem truth : True", "status": "Open"}
        ]}
        if change in {"source_added", "dune_modules"}:
            changed["declarations"].insert(0,
                {"name": "Demo.Extra.extra", "statement": "Theorem extra : True", "status": "Open"}
            )
        elif change == "source_deleted":
            changed["declarations"].clear()
        elif change == "source_restored":
            # The wrapper restores this source on the third process start.
            changed["declarations"].clear()
        elif change == "load_path":
            changed["declarations"][0]["name"] = "Changed.Main.truth"
        elif change == "configuration_changed":
            changed = {
                "kind": "invalid_configuration",
                "message": "_CoqProject load path is incomplete",
            }
        events = [
            event("server_start"),
            event("user_connect", user="alice"),
            command("start", {"project_path": project}, initial),
            command("prove", {"theorem": "truth"}, {
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
            events.append(command("start", {"project_path": project}, initial))
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
        record["implementation"] = "implemented"

def materialize_pending_environment_change(record: dict[str, object]) -> None:
    """Persist a solved candidate, mutate the project, then retry publication."""
    axes = record["axes"]
    assert isinstance(axes, dict)
    change = str(axes["change"])
    recovery = str(axes["recovery"])
    project = {
        "source_added": "project_add",
        "source_same_mtime": "project_content",
        "source_deleted": "project_delete",
        "source_restored": "project_restore",
        "dune_modules": "project_dune",
        "load_path": "project_loadpath",
    }[change]
    initial = {"declarations": [
        {"name": "Demo.Main.truth", "statement": "Theorem truth : True", "status": "Open"}
    ]}
    changed = {"declarations": (
        [
            {"name": "Demo.Extra.extra", "statement": "Theorem extra : True", "status": "Open"},
            {"name": "Demo.Main.truth", "statement": "Theorem truth : True", "status": "Pending"},
        ] if change in {"source_added", "dune_modules"} else [
            {"name": "Demo.Main.truth", "statement": (
                "Theorem truth : False" if change == "source_same_mtime" else "Theorem truth : True"
            ), "status": "Pending"}
        ]
    )}
    if change == "load_path":
        changed = {"declarations": [
            {"name": "Changed.Main.truth", "statement": "Theorem truth : True", "status": "Open"},
            {"name": "Demo.Main.truth", "statement": "Theorem truth : True", "status": "Pending"},
        ]}
    opened = {
        "theorem": "Demo.Main.truth", "statement": "Theorem truth : True",
        "status": "Open", "goals": OPEN_TRUE["goals"],
    }
    events = [
        event("server_start"), event("user_connect", user="alice"),
        command("start", {"project_path": project}, initial),
        command("prove", {"theorem": "truth"}, opened),
        command("check", {"commands": "exact I."}, {
            "state": {**opened, "status": "Pending", "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n"},
            "error": {"kind": "build_timeout", "message": (
                "native build timed out" if change == "dune_modules" else "dependency analysis timed out"
            )},
        }),
    ]
    if recovery == "restart":
        events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
    elif recovery == "reconnect":
        events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
    events.extend([
        command("start", {"project_path": project}, changed),
    ])
    if change == "source_restored":
        if recovery == "restart":
            events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
        events.append(command("start", {"project_path": project}, changed))
    events.extend([
        command("prove", {"theorem": "Demo.Main.truth" if change == "load_path" else "truth"}, (
            {**opened, "status": "Completed", "goals": ""}
            if change in {"source_added", "source_deleted", "source_restored", "dune_modules"} else
            ({"kind": "not_found", "message": "new logical library has no reversible load path"}
             if change == "load_path" else
             {"kind": "declaration_changed", "message": "target declaration changed while proof was open"})
        )),
        event("user_disconnect", user="alice"), event("server_kill"),
    ])
    target = ROOT / str(record["trace"])
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
    record["implementation"] = "implemented"

def materialize_pending_configuration_change(record: dict[str, object]) -> None:
    """A durable candidate survives a temporary invalid project configuration."""
    axes = record["axes"]
    assert isinstance(axes, dict)
    recovery = str(axes["recovery"])
    initial = {"declarations": [
        {"name": "Demo.Main.truth", "statement": "Theorem truth : True", "status": "Open"}
    ]}
    pending = {"declarations": [{**initial["declarations"][0], "status": "Pending"}]}
    opened = {
        "theorem": "Demo.Main.truth", "statement": "Theorem truth : True",
        "status": "Open", "goals": OPEN_TRUE["goals"],
    }
    error = {"kind": "invalid_configuration", "message": "_CoqProject load path is incomplete"}
    events = [
        event("server_start"), event("user_connect", user="alice"),
        command("start", {"project_path": "project_config"}, initial),
        command("prove", {"theorem": "truth"}, opened),
        command("check", {"commands": "exact I."}, {
            "state": {**opened, "status": "Pending", "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n"},
            "error": {"kind": "build_timeout", "message": "dependency analysis timed out"},
        }),
    ]
    if recovery == "restart":
        events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
    elif recovery == "reconnect":
        events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
    if recovery == "same_connection":
        events.append(command("prove", {"theorem": "truth"}, error))
    else:
        events.append(command("start", {"project_path": "project_config"}, error))
    events.extend([
        command("start", {"project_path": "project_config"}, pending),
        command("prove", {"theorem": "truth"}, {**opened, "status": "Completed", "goals": ""}),
        event("user_disconnect", user="alice"), event("server_kill"),
    ])
    target = ROOT / str(record["trace"])
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
    record["implementation"] = "implemented"

def materialize_pending_vo_change(record: dict[str, object]) -> None:
    """Retry a durable proof after its direct external VO changes identity."""
    axes = record["axes"]
    assert isinstance(axes, dict)
    change = str(axes["change"])
    recovery = str(axes["recovery"])
    project, target, proposition, names = {
        "direct_vo_changed": ("project_vo", "before_change", "flag = true", ["before_change", "after_change"]),
        "direct_vo_deleted": ("project_vo_delete", "uses_flag", "flag = true", ["uses_flag"]),
        "transitive_vo_changed": ("project_vo_transitive", "uses_derived", "derived = true", ["uses_derived"]),
        "plugin_vo_changed": ("project_plugin", "before_plugin_change", "plugin_flag = true", ["before_plugin_change", "after_plugin_change"]),
    }[change]
    base = [
        {"name": f"Demo.Main.{name}", "statement": f"Theorem {name} : {proposition}", "status": "Open"}
        for name in names
    ]
    initial = {"declarations": base}
    pending = {"declarations": [{**base[0], "status": "Pending"}, *base[1:]]}
    rejected = {"declarations": [{**base[0], "status": "Rejected"}, *base[1:]]}
    opened = {
        "theorem": f"Demo.Main.{target}",
        "statement": f"Theorem {target} : {proposition}",
        "status": "Open",
        "goals": "focused:\n  ============================\n  " + proposition + "\nunfocused:\nshelved:\ngiven_up:\n",
    }
    events = [
        event("server_start"), event("user_connect", user="alice"),
        command("start", {"project_path": project}, initial),
        command("prove", {"theorem": target}, opened),
        command("check", {"commands": "reflexivity."}, {
            "state": {**opened, "status": "Pending", "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n"},
            "error": {"kind": "build_timeout", "message": "dependency analysis timed out"},
        }),
    ]
    if recovery == "restart":
        events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
    elif recovery == "reconnect":
        events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
    events.extend([
        command("start", {"project_path": project}, pending),
        command("prove", {"theorem": target}, {**opened, "status": "Pending", "goals": ""}),
        command("start", {"project_path": project}, rejected),
        command("prove", {"theorem": target}, {**opened, "status": "Rejected", "goals": ""}),
        event("user_disconnect", user="alice"), event("server_kill"),
    ])
    target = ROOT / str(record["trace"])
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
    record["implementation"] = "implemented"

def materialize_pending_axiom_change(record: dict[str, object]) -> None:
    """A solved proof cannot authorize an axiom introduced after its baseline froze."""
    axes = record["axes"]
    assert isinstance(axes, dict)
    recovery = str(axes["recovery"])
    witness = {
        "name": "Demo.Main.witness",
        "statement": "Definition witness : True := I",
        "status": "Completed",
    }
    truth = {
        "name": "Demo.Main.truth",
        "statement": "Theorem truth : True",
        "status": "Open",
    }
    opened = {
        "theorem": "Demo.Main.truth",
        "statement": "Theorem truth : True",
        "status": "Open",
        "goals": OPEN_TRUE["goals"],
    }
    events = [
        event("server_start"),
        event("user_connect", user="alice"),
        command("start", {"project_path": "project_axiom"}, {"declarations": [witness, truth]}),
        command("prove", {"theorem": "truth"}, opened),
        command("check", {"commands": "exact witness."}, {
            "state": {**opened, "status": "Pending", "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n"},
            "error": {"kind": "build_timeout", "message": "dependency analysis timed out"},
        }),
    ]
    if recovery == "restart":
        events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
    elif recovery == "reconnect":
        events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
    events.extend([
        command("start", {"project_path": "project_axiom"}, {"declarations": [{**truth, "status": "Pending"}]}),
        command("prove", {"theorem": "truth"}, {
            "kind": "axiom_dependency_out_of_scope",
            "message": "axiom dependency is outside the authorized baseline: witness",
        }),
        command("start", {"project_path": "project_axiom"}, {"declarations": [{**truth, "status": "Rejected"}]}),
        command("prove", {"theorem": "truth"}, {**opened, "status": "Rejected", "goals": ""}),
        event("user_disconnect", user="alice"),
        event("server_kill"),
    ])
    target = ROOT / str(record["trace"])
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
    record["implementation"] = "implemented"

def materialize_toolchain_identity_change(record: dict[str, object]) -> None:
    """Exercise changed Rocq/PET executables through the public restart path."""
    axes = record["axes"]
    assert isinstance(axes, dict)
    recovery = str(axes["recovery"])
    candidate = str(axes["candidate"])
    catalog = {"declarations": [
        {"name": "Demo.Main.seed", "statement": "Theorem seed : True", "status": "Completed"},
        {"name": "Demo.Main.truth", "statement": "Theorem truth : True", "status": "Open"},
    ]}
    truth = {
        "theorem": "Demo.Main.truth", "statement": "Theorem truth : True",
        "status": "Open", "goals": OPEN_TRUE["goals"],
    }
    events = [
        event("server_start"), event("user_connect", user="alice"),
        command("start", {"project_path": "project"}, catalog),
        command("prove", {"theorem": "truth"}, truth),
    ]
    if candidate == "solved_pending":
        events.append(command("check", {"commands": "exact I."}, {
            "state": {**truth, "status": "Pending", "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n"},
            "error": {"kind": "build_timeout", "message": "dependency analysis timed out"},
        }))
    if recovery == "restart":
        events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
    elif recovery == "reconnect":
        events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
    events.extend([
        command("start", {"project_path": "project"}, (
            {"declarations": [catalog["declarations"][0], {**catalog["declarations"][1], "status": "Pending"}]}
            if candidate == "solved_pending" else catalog
        )),
        command("prove", {"theorem": "truth"}, (
            {**truth, "status": "Pending", "goals": ""}
            if candidate == "solved_pending" else truth
        )),
        event("user_disconnect", user="alice"), event("server_kill"),
    ])
    target = ROOT / str(record["trace"])
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
    record["implementation"] = "implemented"

def materialize_vo_change(record: dict[str, object], change: str) -> None:
    """Assert a changed external .vo is used by a fresh PET after restart."""
    project, theorem, proposition = {
        "direct_vo_changed": ("project_vo", "before_change", "flag = true"),
        "direct_vo_deleted": ("project_vo_delete", "uses_flag", "flag = true"),
        "transitive_vo_changed": ("project_vo_transitive", "uses_derived", "derived = true"),
        "plugin_vo_changed": ("project_plugin", "before_plugin_change", "plugin_flag = true"),
    }[change]
    names = ["before_change", "after_change"] if change == "direct_vo_changed" else (
        ["before_plugin_change", "after_plugin_change"] if change == "plugin_vo_changed" else ["uses_derived"]
    )
    if change == "direct_vo_deleted":
        names = ["uses_flag"]
    statements = [f"Theorem {name} : {proposition}" for name in names]
    catalog = {"declarations": [
        {"name": f"Demo.Main.{name}", "statement": statement, "status": "Open"}
        for name, statement in zip(names, statements, strict=True)
    ]}
    open_state = {
        "theorem": f"Demo.Main.{theorem}",
        "statement": f"Theorem {theorem} : {proposition}",
        "status": "Open",
        "goals": "focused:\n  ============================\n  " + proposition + "\nunfocused:\nshelved:\ngiven_up:\n",
    }
    lhs = proposition.split(" = ", 1)[0]
    axes = record["axes"]
    assert isinstance(axes, dict)
    recovery = str(axes["recovery"])
    events = [
        event("server_start"), event("user_connect", user="alice"),
        command("start", {"project_path": project}, catalog),
        command("prove", {"theorem": theorem}, open_state),
    ]
    if recovery == "restart":
        events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
    elif recovery == "reconnect":
        events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
    events.append(command("start", {"project_path": project}, catalog))
    if change == "transitive_vo_changed":
        # Mid.vo still points at the old Base.vo identity. A correct fresh PET
        # refuses the stale transitive dependency instead of reusing old state.
        events.append(command("prove", {"theorem": theorem}, {
            "kind": "proof_step_failed",
            "message": "PET rejected request (-32006): Theorem_not_found: [find_thm] Theorem found but failed with Coq error:\n The reference derived was not found in the current environment.!",
        }))
    elif change == "direct_vo_deleted":
        events.append(command("prove", {"theorem": theorem}, {
            "kind": "proof_step_failed", "message": "PET rejected request (-32006): Theorem_not_found: [find_thm] Theorem found but failed with Coq error:\n The reference flag was not found in the current environment.!",
        }))
    else:
        events.extend([
            command("prove", {"theorem": theorem}, open_state),
            command("check", {"commands": "reflexivity."}, {
                "state": open_state,
                "error": {"kind": "proof_step_failed", "message": f'PET rejected request (-32003): Coq: Unable to unify "true" with "{lhs}".'},
            }),
        ])
    events.extend([event("user_disconnect", user="alice"), event("server_kill")])
    target = ROOT / str(record["trace"])
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
    record["implementation"] = "implemented"

def start_failure_is_materialized(axes: dict[str, object]) -> bool:
    return (
        axes["failure"] in {"project_unavailable", "layout_invalid", "layout_ambiguous"}
    ) or axes["failure"] == "project_lock_timeout"

def materialize_start_failures(records: list[dict[str, object]]) -> None:
    """Write each stable failure and its post-disconnect source mutation crossing."""
    selected = []
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if start_failure_is_materialized(axes) and axes["failure"] != "project_lock_timeout":
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
                path = "malformed_coqproject"
                expected = {
                    "kind": "invalid_configuration",
                    "message": "_CoqProject load path is incomplete",
                }
            else:
                path = "ambiguous_project"
                expected = {
                    "kind": "ambiguous",
                    "message": "project layout is ambiguous",
                }
            if axes["source"] == "changed_while_disconnected":
                events.extend([
                    command("start", {"project_path": "project"}, BASIC_CATALOG),
                    event("user_disconnect", user="alice"),
                    event("user_connect", user="alice"),
                    command("start", {"project_path": "project"}, expected),
                ])
            else:
                events.append(command("start", {"project_path": path}, expected))

    write_materialized_family("start_failures", selected, append)
    materialize_start_lock_failures(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["failure"] == "project_lock_timeout"
        ]
    )

def materialize_start_lock_failures(records: list[dict[str, object]]) -> None:
    """Run project lock timeout and public recovery in a fresh process lab."""
    directory = ROOT / "traces/project_timeout/generated/start_failures"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    catalog = {
        "declarations": [
            {
                "name": "Demo.Main.truth",
                "statement": "Theorem truth : True",
                "status": "Open",
            }
        ]
    }
    timeout = {
        "kind": "project_timeout",
        "message": "timed out waiting for exclusive project ownership",
    }
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        lifecycle = str(axes["lifecycle"])
        events = [event("server_start"), event("user_connect", user="alice")]
        events.append(command("start", {"project_path": "project"}, timeout))
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
        events.append(command("start", {"project_path": "project"}, catalog))
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        (ROOT / str(record["trace"])).write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )
        record["implementation"] = "implemented"
        record["implementation"] = "implemented"
