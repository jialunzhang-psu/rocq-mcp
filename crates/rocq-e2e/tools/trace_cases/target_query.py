"""Target-query and status-dependent proof trace scenarios."""

from __future__ import annotations

import json
import shutil

from .matrix import ROOT
from .common import OPEN_TRUE, command, event, invalid_request, lifecycle_prefix, user_command
from .check import CHECK_SHAPES, check_timeout_catalog


QUERY_KIND_INFO = {
    "Theorem": {
        "open": ("OT", "open_theorem", "True"),
        "done": ("DT", "done_theorem", "True"),
        "duplicate": "dup_theorem",
    },
    "Lemma": {
        "open": ("OL", "open_lemma", "True"),
        "done": ("DL", "done_lemma", "True"),
        "duplicate": "dup_lemma",
    },
    "Definition": {
        "open": ("OD", "open_definition", "nat"),
        "done": ("DD", "done_definition", "nat"),
        "duplicate": "dup_definition",
    },
}

QUERY_CATALOG = {
    "declarations": [
        {
            "name": f"Query.Main.{module}.{constant}",
            "statement": f"{kind} {constant} : {ty}",
            "status": status,
        }
        for module, kind, constant, ty, status in (
            ("OT", "Theorem", "open_theorem", "True", "Open"),
            ("OL", "Lemma", "open_lemma", "True", "Open"),
            ("OD", "Definition", "open_definition", "nat", "Open"),
            ("DT", "Theorem", "done_theorem", "True", "Completed"),
            ("DL", "Lemma", "done_lemma", "True", "Completed"),
            ("DD", "Definition", "done_definition", "nat", "Completed"),
            ("T1", "Theorem", "dup_theorem", "True", "Open"),
            ("T2", "Theorem", "dup_theorem", "True", "Open"),
            ("L1", "Lemma", "dup_lemma", "True", "Open"),
            ("L2", "Lemma", "dup_lemma", "True", "Open"),
            ("D1", "Definition", "dup_definition", "nat", "Open"),
            ("D2", "Definition", "dup_definition", "nat", "Open"),
        )
    ]
}

QUERY_OPEN = {
    "theorem": "Query.Main.OT.open_theorem",
    "statement": "Theorem open_theorem : True",
    "status": "Open",
    "goals": OPEN_TRUE["goals"],
}

QUERY_DONE = {
    "theorem": "Query.Main.DT.done_theorem",
    "statement": "Theorem done_theorem : True",
    "status": "Completed",
    "goals": "",
}

def query_selection_setup(selection: str) -> list[dict[str, object]]:
    if selection == "no_project":
        return []
    setup = [command("start", {"project_path": "query_project"}, QUERY_CATALOG)]
    if selection == "open":
        setup.append(command("prove", {"theorem": "OT.open_theorem"}, QUERY_OPEN))
    elif selection == "completed":
        setup.append(command("prove", {"theorem": "DT.done_theorem"}, QUERY_DONE))
    return setup

def query_lifecycle_prefix(selection: str, lifecycle: str) -> list[dict[str, object]]:
    events = [event("server_start"), event("user_connect", user="alice")]
    events.extend(query_selection_setup(selection))
    if lifecycle == "reconnect":
        events.extend(
            [event("user_disconnect", user="alice"), event("user_connect", user="alice")]
        )
        events.extend(query_selection_setup(selection))
    elif lifecycle == "restart":
        events.extend(
            [event("server_kill"), event("server_start"), event("user_connect", user="alice")]
        )
        events.extend(query_selection_setup(selection))
    return events

def query_target_name(target: str, declaration_kind: str) -> str:
    info = QUERY_KIND_INFO[declaration_kind]
    if target == "open_short":
        return f"{info["open"][0]}.{info["open"][1]}"
    if target in {"completed_short", "module_suffix", "full_name"}:
        module, constant, _ = info["done"]
        if target == "completed_short":
            return str(constant)
        if target == "module_suffix":
            return f"{module}.{constant}"
        return f"Query.Main.{module}.{constant}"
    if target == "missing":
        return f"missing_{declaration_kind.lower()}"
    if target == "ambiguous":
        return str(info["duplicate"])
    raise AssertionError(target)

def query_target_expected(kind: str, target: str, declaration_kind: str) -> object:
    requested = query_target_name(target, declaration_kind)
    if target == "missing":
        return {"kind": "not_found", "message": f"declaration '{requested}' was not found"}
    if target == "ambiguous":
        return {"kind": "ambiguous", "message": "declaration name is ambiguous"}
    info = QUERY_KIND_INFO[declaration_kind]
    phase = "open" if target == "open_short" else "done"
    module, constant, ty = info[phase]
    header = f"{declaration_kind} {constant} : {ty}"
    if kind == "statement":
        return {"text": header}
    if kind in {"proof", "definition"}:
        ending = "Admitted." if phase == "open" else (
            "Proof. exact 42. Defined."
            if declaration_kind == "Definition"
            else "Proof. exact I. Qed."
        )
        return {"text": f"\n{header}. {ending}"}
    if phase == "open":
        return {"text": f'[3,"Axioms:\\n{module}.{constant} : {ty}"]'}
    if kind == "assumptions":
        return {"text": '[3,"Closed under the global context"]'}
    transparency = "Transparent" if declaration_kind == "Definition" else "Opaque"
    return {
        "text": f'[3,"{transparency} constants:\\n{module}.{constant} :\\n{ty}"]'
    }

def materialize_query_target(records: list[dict[str, object]]) -> None:
    """Write source-backed target resolution except Pending/Rejected setup cases."""
    directory = ROOT / "traces/basic/generated/query_target"
    if directory.exists():
        shutil.rmtree(directory)
    selected = [
        record
        for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["target"] not in {"pending", "rejected"}
    ]
    grouped: dict[str, list[dict[str, object]]] = {}
    for record in selected:
        grouped.setdefault(str(record["trace"]), []).append(record)
    for relative, members in grouped.items():
        members.sort(key=lambda record: int(record["case_index"]))
        axes = members[0]["axes"]
        assert isinstance(axes, dict)
        selection = str(axes["selection"])
        events = query_lifecycle_prefix(selection, str(axes["lifecycle"]))
        for record in members:
            case_axes = record["axes"]
            assert isinstance(case_axes, dict)
            kind = str(case_axes["kind"])
            target = str(case_axes["target"])
            declaration_kind = str(case_axes["declaration_kind"])
            expected = (
                invalid_request("call start first")
                if selection == "no_project"
                else query_target_expected(kind, target, declaration_kind)
            )
            events.append(
                command(
                    "query",
                    {"kind": kind, "target": query_target_name(target, declaration_kind)},
                    expected,
                )
            )
            record["implementation"] = "implemented"
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        path = ROOT / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )
    materialize_query_target_statuses(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["target"] in {"pending", "rejected"}
        ]
    )

REJECTED_CATALOG = {
    "declarations": [
        {
            "name": "Rejected.Main.unfinished",
            "statement": "Theorem unfinished : True",
            "status": "Open",
        },
        *[
            {
                "name": f"Rejected.Main.rejected_{kind.lower()}",
                "statement": f"{kind} rejected_{kind.lower()} : True",
                "status": "Open",
            }
            for kind in ("Theorem", "Lemma", "Definition")
        ],
        {
            "name": "Rejected.Main.open_anchor",
            "statement": "Theorem open_anchor : True",
            "status": "Open",
        },
        {
            "name": "Rejected.Main.done_true",
            "statement": "Theorem done_true : True",
            "status": "Completed",
        },
    ]
}

def status_target_name(status: str, declaration_kind: str) -> str:
    if status == "pending":
        return f"timeout_true_{declaration_kind.lower()}"
    return f"rejected_{declaration_kind.lower()}"

def status_catalog(status: str, declaration_kind: str) -> dict[str, object]:
    catalog = check_timeout_catalog("coqproject") if status == "pending" else REJECTED_CATALOG
    target = status_target_name(status, declaration_kind)
    declarations = [dict(item) for item in catalog["declarations"]]
    for item in declarations:
        if str(item["name"]).endswith(f".{target}"):
            item["status"] = "Pending" if status == "pending" else "Rejected"
    return {"declarations": declarations}

def status_target_state(
    status: str, declaration_kind: str, lifecycle: str = "Open"
) -> dict[str, object]:
    target = status_target_name(status, declaration_kind)
    library = "Matrix.Main" if status == "pending" else "Rejected.Main"
    return {
        "theorem": f"{library}.{target}",
        "statement": f"{declaration_kind} {target} : True",
        "status": lifecycle,
        "goals": (
            OPEN_TRUE["goals"]
            if lifecycle == "Open"
            else "focused:\nunfocused:\nshelved:\ngiven_up:\n"
        ),
    }

def status_selection_setup(
    events: list[dict[str, object]], status: str, declaration_kind: str, selection: str
) -> None:
    if selection == "no_project":
        return
    project = "matrix_project" if status == "pending" else "project"
    events.append(
        user_command(
            "alice",
            "start",
            {"project_path": project},
            status_catalog(status, declaration_kind),
        )
    )
    if selection == "open":
        if status == "pending":
            state = {
                "theorem": "Matrix.Main.timeout_conjunction_theorem",
                "statement": "Theorem timeout_conjunction_theorem : True /\\ True",
                "status": "Open",
                "goals": CHECK_SHAPES["conjunction"]["goals"],
            }
            target = "timeout_conjunction_theorem"
        else:
            state = {
                "theorem": "Rejected.Main.open_anchor",
                "statement": "Theorem open_anchor : True",
                "status": "Open",
                "goals": OPEN_TRUE["goals"],
            }
            target = "open_anchor"
        events.append(user_command("alice", "prove", {"theorem": target}, state))
    elif selection == "completed":
        library = "Matrix.Main" if status == "pending" else "Rejected.Main"
        events.append(
            user_command(
                "alice",
                "prove",
                {"theorem": "done_true"},
                {
                    "theorem": f"{library}.done_true",
                    "statement": "Theorem done_true : True",
                    "status": "Completed",
                    "goals": "",
                },
            )
        )

def status_query_expected(
    status: str, kind: str, declaration_kind: str, selection: str
) -> object:
    if selection == "no_project":
        return invalid_request("call start first")
    target = status_target_name(status, declaration_kind)
    header = f"{declaration_kind} {target} : True"
    if kind == "statement":
        return {"text": header}
    if kind in {"proof", "definition"}:
        prefix = "" if status == "pending" and declaration_kind == "Theorem" else "\n"
        return {"text": f"{prefix}{header}. Admitted."}
    return {"text": f'[3,"Axioms:\\n{target} : True"]'}

def status_scenario_prefix(
    status: str, declaration_kind: str, lifecycle: str, selection: str
) -> tuple[list[dict[str, object]], bool]:
    """Create one durable status with Bob, then establish Alice's selection.

    Returns the event prefix and whether Bob remains connected for teardown.
    This is the single setup implementation shared by status queries and prove.
    """
    project = "matrix_project" if status == "pending" else "project"
    initial_catalog = (
        check_timeout_catalog("coqproject")
        if status == "pending"
        else REJECTED_CATALOG
    )
    target = status_target_name(status, declaration_kind)
    events = [event("server_start"), event("user_connect", user="bob")]
    events.append(user_command("bob", "start", {"project_path": project}, initial_catalog))
    events.append(
        user_command(
            "bob",
            "prove",
            {"theorem": target},
            status_target_state(status, declaration_kind),
        )
    )
    if status == "pending":
        events.append(
            user_command(
                "bob",
                "check",
                {"commands": "exact I."},
                {
                    "state": status_target_state(status, declaration_kind, "Pending"),
                    "error": {
                        "kind": "build_timeout",
                        "message": "dependency analysis timed out",
                    },
                },
            )
        )
    else:
        events.append(
            user_command(
                "bob",
                "check",
                {"commands": "exact unfinished."},
                {
                    "state": status_target_state(status, declaration_kind, "Rejected"),
                    "error": {
                        "kind": "unfinished_dependency",
                        "message": "proof depends on unfinished local declaration 'unfinished'",
                    },
                },
            )
        )
    if lifecycle == "restart":
        events.extend(
            [
                event("server_kill"),
                event("server_start"),
                event("user_connect", user="alice"),
            ]
        )
        bob_connected = False
    else:
        events.append(event("user_connect", user="alice"))
        bob_connected = True
        if lifecycle == "reconnect":
            events.extend(
                [
                    event("user_disconnect", user="alice"),
                    event("user_connect", user="alice"),
                ]
            )
    status_selection_setup(events, status, declaration_kind, selection)
    return events, bob_connected

def finish_status_scenario(
    events: list[dict[str, object]], bob_connected: bool
) -> None:
    events.append(event("user_disconnect", user="alice"))
    if bob_connected:
        events.append(event("user_disconnect", user="bob"))
    events.append(event("server_kill"))

def materialize_prove_statuses(records: list[dict[str, object]]) -> None:
    """Open durable Pending/Rejected targets through the public prove command."""
    for status in ("pending", "rejected"):
        fixture = "check_timeout" if status == "pending" else "query_rejected"
        directory = ROOT / f"traces/{fixture}/generated/prove_{status}"
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        status = str(axes["target"])
        selection = str(axes["selection"])
        events, bob_connected = status_scenario_prefix(
            status, "Theorem", str(axes["lifecycle"]), selection
        )
        target = status_target_name(status, "Theorem")
        args: dict[str, object] = {"theorem": target}
        if axes["extra"] == "present":
            args["extra"] = True
            expected: object = invalid_request("unexpected field 'extra'")
        elif selection == "no_project":
            expected = invalid_request("call start first")
        elif status == "pending":
            expected = {
                **status_target_state(status, "Theorem", "Completed"),
                "goals": "",
            }
        else:
            expected = {
                **status_target_state(status, "Theorem", "Rejected"),
                "goals": "",
            }
        events.append(user_command("alice", "prove", args, expected))
        finish_status_scenario(events, bob_connected)
        (ROOT / str(record["trace"])).write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )
        record["implementation"] = "implemented"

def append_status_runtime_trigger(
    events: list[dict[str, object]], status: str, declaration_kind: str, runtime: str
) -> bool:
    """Realize the requested PET runtime condition without changing Alice's selection."""
    if runtime == "pet_killed":
        # Bob's status creation already crossed the odd-spawn death/retry proxy.
        return False
    events.append(event("user_connect", user="carol"))
    if runtime == "pet_evicted":
        catalog = {
            "declarations": [
                {
                    "name": "RuntimeC.Main.truth_c",
                    "statement": "Theorem truth_c : True",
                    "status": "Open",
                }
            ]
        }
        state = {
            "theorem": "RuntimeC.Main.truth_c",
            "statement": "Theorem truth_c : True",
            "status": "Open",
            "goals": OPEN_TRUE["goals"],
        }
        events.append(
            user_command(
                "carol", "start", {"project_path": "other_project"}, catalog
            )
        )
        events.append(
            user_command("carol", "prove", {"theorem": "truth_c"}, state)
        )
        return True
    assert runtime == "proof_timeout"
    project = "matrix_project" if status == "pending" else "project"
    events.append(
        user_command(
            "carol",
            "start",
            {"project_path": project},
            status_catalog(status, declaration_kind),
        )
    )
    if status == "pending":
        target = "timeout_conjunction_theorem"
        state = {
            "theorem": "Matrix.Main.timeout_conjunction_theorem",
            "statement": "Theorem timeout_conjunction_theorem : True /\\ True",
            "status": "Open",
            "goals": CHECK_SHAPES["conjunction"]["goals"],
        }
    else:
        target = "open_anchor"
        state = {
            "theorem": "Rejected.Main.open_anchor",
            "statement": "Theorem open_anchor : True",
            "status": "Open",
            "goals": OPEN_TRUE["goals"],
        }
    events.append(user_command("carol", "prove", {"theorem": target}, state))
    events.append(
        user_command(
            "carol",
            "check",
            {"commands": "let rec loop n := loop n in loop 0."},
            {
                "state": state,
                "error": {
                    "kind": "proof_timeout",
                    "message": "PET operation timed out",
                },
            },
        )
    )
    return True

def materialize_prove_status_runtimes(records: list[dict[str, object]]) -> None:
    """Cross durable status reopening with death, eviction, and prior timeout."""
    root = ROOT / "traces/status_runtime/generated"
    for runtime in ("pet_killed", "pet_evicted", "proof_timeout"):
        directory = root / f"prove_{runtime}"
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        status = str(axes["target"])
        selection = str(axes["selection"])
        runtime = str(axes["runtime"])
        events, bob_connected = status_scenario_prefix(
            status, "Theorem", str(axes["lifecycle"]), selection
        )
        carol_connected = append_status_runtime_trigger(
            events, status, "Theorem", runtime
        )
        target = status_target_name(status, "Theorem")
        args: dict[str, object] = {"theorem": target}
        if axes["extra"] == "present":
            args["extra"] = True
            expected: object = invalid_request("unexpected field 'extra'")
        elif selection == "no_project":
            expected = invalid_request("call start first")
        elif status == "pending":
            expected = {
                **status_target_state(status, "Theorem", "Completed"),
                "goals": "",
            }
        else:
            expected = {
                **status_target_state(status, "Theorem", "Rejected"),
                "goals": "",
            }
        events.append(user_command("alice", "prove", args, expected))
        if carol_connected:
            events.append(event("user_disconnect", user="carol"))
        finish_status_scenario(events, bob_connected)
        target_path = ROOT / str(record["trace"])
        target_path.parent.mkdir(parents=True, exist_ok=True)
        target_path.write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )
        record["implementation"] = "implemented"

def materialize_query_target_statuses(records: list[dict[str, object]]) -> None:
    """Create real Pending/Rejected targets, then query them from each selection."""
    for status in ("pending", "rejected"):
        fixture = "check_timeout" if status == "pending" else "query_rejected"
        directory = ROOT / f"traces/{fixture}/generated/query_target_{status}"
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
    grouped: dict[str, list[dict[str, object]]] = {}
    for record in records:
        grouped.setdefault(str(record["trace"]), []).append(record)
    for relative, members in grouped.items():
        members.sort(key=lambda item: int(item["case_index"]))
        axes = members[0]["axes"]
        assert isinstance(axes, dict)
        status = str(axes["target"])
        declaration_kind = str(axes["declaration_kind"])
        lifecycle = str(axes["lifecycle"])
        selection = str(axes["selection"])
        target = status_target_name(status, declaration_kind)
        events, bob_connected = status_scenario_prefix(
            status, declaration_kind, lifecycle, selection
        )
        for record in members:
            case_axes = record["axes"]
            assert isinstance(case_axes, dict)
            kind = str(case_axes["kind"])
            events.append(
                user_command(
                    "alice",
                    "query",
                    {"kind": kind, "target": target},
                    status_query_expected(status, kind, declaration_kind, selection),
                )
            )
            record["implementation"] = "implemented"
        finish_status_scenario(events, bob_connected)
        (ROOT / relative).write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )

def query_kind_args(kind: str, extra: str) -> dict[str, object]:
    args: dict[str, object] = {}
    if kind == "null":
        args["kind"] = None
    elif kind == "wrong_type":
        args["kind"] = 0
    elif kind != "missing":
        args["kind"] = "" if kind == "empty" else kind
    if extra == "present":
        args["extra"] = True
    return args

def query_kind_expected(kind: str, extra: str, selection: str) -> object:
    if selection == "no_project":
        return invalid_request("call start first")
    if kind in {"missing", "null", "wrong_type"}:
        return invalid_request("kind is required")
    if extra == "present":
        return invalid_request("unexpected field 'extra'")
    if kind == "goals":
        return OPEN_TRUE if selection == "open" else invalid_request("goals requires attempt")
    if kind == "search":
        return {
            "text": (
                "Demo.Main.open_true\nDemo.Main.done_true\nDemo.Main.answer\n"
                "Demo.Main.duplicate\nDemo.Main.Nested.duplicate"
            )
        }
    if kind in {"statement", "proof", "definition", "assumptions", "dependencies"}:
        return invalid_request("target must be a non-empty string")
    if kind in {"type", "notations"}:
        return invalid_request("expression must be a non-empty string")
    return invalid_request("unsupported query kind")

def materialize_query_kind(records: list[dict[str, object]]) -> None:
    """Write the query discriminator product as twelve aggregated traces."""
    directory = ROOT / "traces/basic/generated/query_kind"
    # This generator is the sole owner of this directory; delete stale shards
    # so a grouping change cannot silently leave extra executable traces.
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
        selection = str(axes["selection"])
        lifecycle = str(axes["lifecycle"])
        events = lifecycle_prefix(selection, lifecycle)
        for record in members:
            case_axes = record["axes"]
            assert isinstance(case_axes, dict)
            kind = str(case_axes["kind"])
            extra = str(case_axes["extra"])
            events.append(
                command(
                    "query",
                    query_kind_args(kind, extra),
                    query_kind_expected(kind, extra, selection),
                )
            )
            record["implementation"] = "implemented"
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        target = ROOT / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )
