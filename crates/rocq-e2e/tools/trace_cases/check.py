"""Check, check_multi, and publication-result scenarios."""

from __future__ import annotations

import json
import shutil

from .matrix import ROOT, check_case_needs_build_timeout_fixture
from .common import OPEN_TRUE, PROOF_OPEN, command, event, invalid_request, write_materialized_family


PROOF_OPEN_AFTER_IDTAC = PROOF_OPEN.copy()
PROOF_OPEN_SOLVED = {
    **PROOF_OPEN,
    "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n",
}


def sized_idtac(length: int) -> str:
    """Return one complete idtac sentence with exactly ``length`` bytes."""
    return 'idtac "' + "a" * (length - 9) + '".'

def check_multi_candidates(value: str) -> object:
    if value == "null":
        return None
    if value == "wrong_container":
        return "exact I."
    if value == "wrong_item":
        return [0]
    if value == "empty_item":
        return [""]
    if value == "two_sentences":
        return ["idtac. exact I."]
    if value.startswith("count_"):
        count = int(value.removeprefix("count_"))
        return ["exact I."] * count
    if value == "at_limit":
        return [sized_idtac(4096)]
    if value == "over_limit":
        return [sized_idtac(4097)]
    if value == "duplicate":
        return ["idtac.", "idtac."]
    if value == "mixed":
        return ["idtac.", "exact I."]
    raise AssertionError(value)

def check_multi_args(value: str, extra: str) -> dict[str, object]:
    args: dict[str, object] = {}
    if value != "missing":
        args["candidates"] = check_multi_candidates(value)
    if extra == "present":
        args["extra"] = True
    return args

def candidate_result(solved: bool) -> dict[str, object]:
    return {
        "solved": solved,
        "state": PROOF_OPEN_SOLVED if solved else PROOF_OPEN_AFTER_IDTAC,
        "error": None,
    }

def check_multi_parameters_expected(
    value: str, extra: str, selection: str
) -> object:
    if extra == "present":
        return invalid_request("unexpected field 'extra'")
    if selection != "open":
        return invalid_request("call prove first")
    if value in {"missing", "null", "wrong_container"}:
        return invalid_request("candidates must be an array")
    if value == "wrong_item":
        return invalid_request("candidate must be a string")
    if value in {"empty_item", "two_sentences"}:
        return invalid_request("candidate must be one sentence")
    if value in {"count_0", "count_21"}:
        return invalid_request("candidate count is outside range")
    if value == "over_limit":
        return invalid_request("candidate is too large")
    if value == "at_limit":
        return {"candidates": [candidate_result(False)]}
    if value == "duplicate":
        return {"candidates": [candidate_result(False), candidate_result(False)]}
    if value == "mixed":
        return {"candidates": [candidate_result(False), candidate_result(True)]}
    count = int(value.removeprefix("count_"))
    return {"candidates": [candidate_result(True) for _ in range(count)]}

def materialize_check_multi_parameters(records: list[dict[str, object]]) -> None:
    """Write candidate-container, count and payload-boundary crossings."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            value = str(axes["candidates"])
            extra = str(axes["extra"])
            selection = str(axes["selection"])
            events.append(
                command(
                    "check_multi",
                    check_multi_args(value, extra),
                    check_multi_parameters_expected(value, extra, selection),
                )
            )

    write_materialized_family("check_multi_parameters", records, append)

PAIR_OPEN = {
    "theorem": "Matrix.Main.pair",
    "statement": "Theorem pair : True /\\ True",
    "status": "Open",
    "goals": (
        "focused:\n  ============================\n  True /\\ True\n"
        "unfocused:\nshelved:\ngiven_up:\n"
    ),
}

PAIR_UNSOLVED = {
    **PAIR_OPEN,
    "goals": (
        "focused:\n  ============================\n  True\n"
        "  ============================\n  True\n"
        "unfocused:\nshelved:\ngiven_up:\n"
    ),
}

PAIR_SOLVED = {
    **PAIR_OPEN,
    "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n",
}

PAIR_FAILED = {
    "solved": False,
    "state": None,
    "error": {
        "kind": "proof_step_failed",
        "message": (
            'PET rejected request (-32003): Coq: The term "I" has type "True" '
            'while it is expected to have type\n "True /\\ True".'
        ),
    },
}

ORDER_ITEMS = {
    "unsolved": ("split.", {"solved": False, "state": PAIR_UNSOLVED, "error": None}),
    "failed": ("exact I.", PAIR_FAILED),
    "solved": (
        "exact (conj I I).",
        {"solved": True, "state": PAIR_SOLVED, "error": None},
    ),
}

def materialize_check_multi_order(records: list[dict[str, object]]) -> None:
    """Write all observable candidate-result permutations."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        axes = members[0]["axes"]
        assert isinstance(axes, dict)
        selection = str(axes["selection"])
        if selection == "open":
            events.append(command("prove", {"theorem": "pair"}, PAIR_OPEN))
        for record in members:
            case_axes = record["axes"]
            assert isinstance(case_axes, dict)
            order = str(case_axes["order"]).split("_")
            extra = str(case_axes["extra"])
            args: dict[str, object] = {
                "candidates": [ORDER_ITEMS[item][0] for item in order]
            }
            if extra == "present":
                args["extra"] = True
            if extra == "present":
                expected: object = invalid_request("unexpected field 'extra'")
            elif selection != "open":
                expected = invalid_request("call prove first")
            else:
                expected = {"candidates": [ORDER_ITEMS[item][1] for item in order]}
            events.append(command("check_multi", args, expected))

    write_materialized_family("check_multi_order", records, append)

def check_multi_failure_is_materialized(axes: dict[str, object]) -> bool:
    return axes["failure"] in {"proof_timeout", "invalid_configuration", "declaration_changed"}

def materialize_check_multi_failures(records: list[dict[str, object]]) -> None:
    """Write candidate-local PET timeout results; environment faults stay separate."""
    selected = []
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if axes["failure"] == "proof_timeout":
            selected.append(record)

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for _record in members:
            events.append(
                command(
                    "check_multi",
                    {"candidates": ["let rec loop n := loop n in loop 0."]},
                    {
                        "candidates": [
                            {
                                "solved": False,
                                "state": None,
                                "error": {
                                    "kind": "proof_timeout",
                                    "message": "PET operation timed out",
                                },
                            }
                        ]
                    },
                )
            )

    write_materialized_family("check_multi_failures", selected, append)
    materialize_check_multi_configuration_failures([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["failure"] == "invalid_configuration"
    ])
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if axes["failure"] == "declaration_changed":
            materialize_check_multi_declaration_change(record)

def materialize_check_multi_declaration_change(record: dict[str, object]) -> None:
    """Change the selected theorem header after prove and check candidate replay."""
    relative = str(record["trace"])
    axes = record["axes"]
    assert isinstance(axes, dict)
    lifecycle = str(axes["lifecycle"])
    opened = {
        "theorem": "Demo.Main.truth",
        "statement": "Theorem truth : True",
        "status": "Open",
        "goals": OPEN_TRUE["goals"],
    }
    start = command("start", {"project_path": "project_content"}, {"declarations": [{
        "name": "Demo.Main.truth", "statement": "Theorem truth : True", "status": "Open",
    }]})
    events = [
        event("server_start"),
        event("user_connect", user="alice"),
        start,
    ]
    if lifecycle == "reconnect":
        events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice"), start])
    elif lifecycle == "restart":
        events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice"), start])
    events.extend([
        command("prove", {"theorem": "truth"}, opened),
        command("check_multi", {"candidates": ["exact I."]}, {"candidates": [{
            "solved": False,
            "state": None,
            "error": {"kind": "declaration_changed", "message": "invalid PET request: declaration interface changed"},
        }]}),
        event("user_disconnect", user="alice"),
        event("server_kill"),
    ])
    target = ROOT / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
    record["implementation"] = "implemented"

def materialize_check_multi_configuration_failures(records: list[dict[str, object]]) -> None:
    """Return a candidate-local error after a selected project's load path breaks."""
    directory = ROOT / "traces/source_change/generated/check_multi_failures"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    catalog = {"declarations": [
        {"name": "Demo.Main.truth", "statement": "Theorem truth : True", "status": "Open"}
    ]}
    opened = {
        "theorem": "Demo.Main.truth", "statement": "Theorem truth : True",
        "status": "Open", "goals": OPEN_TRUE["goals"],
    }
    expected = {"candidates": [{
        "solved": False, "state": None,
        "error": {"kind": "invalid_configuration", "message": "invalid PET request: _CoqProject load path is incomplete"},
    }]}
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        lifecycle = str(axes["lifecycle"])
        events = [event("server_start"), event("user_connect", user="alice")]
        if lifecycle != "connected":
            events.append(command("start", {"project_path": "project_config"}, catalog))
            if lifecycle == "reconnect":
                events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
            else:
                events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
        events.extend([
            command("start", {"project_path": "project_config"}, catalog),
            command("prove", {"theorem": "truth"}, opened),
            command("check_multi", {"candidates": ["idtac."]}, expected),
            event("user_disconnect", user="alice"), event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
        record["implementation"] = "implemented"

ESCAPING_COMMANDS = {
    "Qed": "Qed.",
    "Defined": "Defined.",
    "Admitted": "Admitted.",
    "Abort": "Abort.",
    "Save": "Save escaped.",
    "Restart": "Restart.",
    "Undo": "Undo.",
    "Undo_all": "Undo All.",
    "Back": "Back.",
    "Reset": "Reset escaped.",
    "Reset_Initial": "Reset Initial.",
    "Drop": "Drop.",
    "Focus": "Focus.",
    "Unfocus": "Unfocus.",
    "Show": "Show.",
    "Show_Proof": "Show Proof.",
    "Show_Script": "Show Script.",
    "Guarded": "Guarded.",
    "Proof": "Proof.",
    "Theorem": "Theorem escaped : True.",
    "Lemma": "Lemma escaped : True.",
    "Definition": "Definition escaped : True.",
    "Fixpoint": "Fixpoint escaped := 0.",
    "CoFixpoint": "CoFixpoint escaped : nat := 0.",
}

def materialize_check_escaping_heads(records: list[dict[str, object]]) -> None:
    """Write proof-control and declaration heads, including legal focus tactics."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            args: dict[str, object] = {"commands": ESCAPING_COMMANDS[str(axes["head"])]}
            if axes["extra"] == "present":
                args["extra"] = True
            if axes["extra"] == "present":
                expected: object = invalid_request("unexpected field 'extra'")
            elif axes["selection"] != "open":
                expected = invalid_request("call prove first")
            elif axes["head"] in ("Focus", "Unfocus"):
                expected = {"state": {
                    "theorem": "Matrix.Main.truth",
                    "statement": "Theorem truth : True",
                    "status": "Open",
                    "goals": OPEN_TRUE["goals"],
                }, "error": None}
            else:
                expected = invalid_request("proof-escaping vernacular is not a tactic")
            events.append(command("check", args, expected))

    write_materialized_family("check_escaping_heads", records, append)

CHECK_SHAPES = {
    "True": {
        "statement": "True",
        "goals": "focused:\n  ============================\n  True\nunfocused:\nshelved:\ngiven_up:\n",
        "solved": "exact I.",
    },
    "conjunction": {
        "statement": "True /\\ True",
        "goals": "focused:\n  ============================\n  True /\\ True\nunfocused:\nshelved:\ngiven_up:\n",
        "solved": "exact (conj I I).",
    },
    "forall": {
        "statement": "forall n : nat, n = n",
        "goals": "focused:\n  ============================\n  forall n : nat, n = n\nunfocused:\nshelved:\ngiven_up:\n",
        "solved": "intros n; reflexivity.",
    },
    "Definition": {
        "statement": "nat",
        "goals": "focused:\n  ============================\n  nat\nunfocused:\nshelved:\ngiven_up:\n",
        "solved": "exact 0.",
    },
}

def check_case_is_materialized(axes: dict[str, object]) -> bool:
    """All check cases have either an ordinary or isolated build-fault trace."""
    _ = axes
    return True

def check_args(value: str, extra: str) -> dict[str, object]:
    args: dict[str, object] = {}
    if value == "null":
        args["commands"] = None
    elif value == "wrong_type":
        args["commands"] = 0
    elif value == "empty":
        args["commands"] = ""
    elif value == "whitespace":
        args["commands"] = "   "
    elif value == "missing_dot":
        args["commands"] = "idtac"
    elif value == "unterminated_comment":
        args["commands"] = "(*"
    elif value == "unterminated_string":
        args["commands"] = 'idtac "unterminated.'
    elif value == "unsolved":
        args["commands"] = "idtac."
    elif value == "given_up":
        args["commands"] = "solve [admit]."
    elif value == "partial_failure":
        # The first sentence is deliberately retained before the second fails.
        args["commands"] = "idtac. fail."
    elif value == "at_limit":
        args["commands"] = 'idtac "' + ("a" * (4096 - len('idtac "".'))) + '".'
    elif value == "over_limit":
        args["commands"] = 'idtac "' + ("a" * (4097 - len('idtac "".'))) + '".'
    elif value == "proof_timeout":
        args["commands"] = "let rec loop n := loop n in loop 0."
    elif value == "build_timeout":
        # Only state-gated/unknown-field variants reach the ordinary corpus.
        args["commands"] = "exact I."
    elif value != "missing":
        assert value == "solved"
        # Filled from the proof-shape table by materialize_check.
    if extra == "present":
        args["extra"] = True
    return args

def check_state(
    record: dict[str, object], axes: dict[str, object], status: str = "Open"
) -> dict[str, object]:
    shape = CHECK_SHAPES[str(axes["proof_shape"])]
    layout = str(axes["layout"])
    library = "Matrix.Main" if layout == "coqproject" else "MatrixDune.Main"
    name = f"check_{record['case']}"
    return {
        "theorem": f"{library}.{name}",
        "statement": f"{axes['declaration_kind']} {name} : {shape['statement']}",
        "status": status,
        "goals": "" if status == "Completed" else shape["goals"],
    }

def check_declare(
    record: dict[str, object], axes: dict[str, object]
) -> dict[str, object]:
    shape = CHECK_SHAPES[str(axes["proof_shape"])]
    return command(
        "declare",
        {
            "name": f"check_{record['case']}",
            "statement": shape["statement"],
            "kind": axes["declaration_kind"],
        },
        check_state(record, axes),
    )

def check_expected(
    record: dict[str, object], axes: dict[str, object]
) -> object:
    if axes["extra"] == "present":
        return invalid_request("unexpected field 'extra'")
    if axes["selection"] != "open":
        return invalid_request("call prove first")
    value = str(axes["commands"])
    if value in {"missing", "null", "wrong_type", "empty"}:
        return invalid_request("commands must be a non-empty string")
    if value == "whitespace":
        return invalid_request("empty tactic input")
    if value == "missing_dot":
        return invalid_request("tactic must be one complete sentence")
    if value in {"unterminated_comment", "unterminated_string"}:
        return invalid_request("malformed tactic input")
    if value == "over_limit":
        return invalid_request("tactic is too large")
    state = check_state(record, axes, "Completed" if value == "solved" else "Open")
    if value == "given_up":
        statement = CHECK_SHAPES[str(axes["proof_shape"])]["statement"]
        state["goals"] = (
            "focused:\nunfocused:\nshelved:\ngiven_up:\n"
            f"  ============================\n  {statement}\n"
        )
    error: object = None
    if value == "partial_failure":
        error = {
            "kind": "proof_step_failed",
            "message": "PET rejected request (-32003): Coq: Tactic failure.",
        }
    elif value == "proof_timeout":
        error = {"kind": "proof_timeout", "message": "PET operation timed out"}
    return {"state": state, "error": error}

def materialize_check(records: list[dict[str, object]]) -> None:
    """Write every deterministic check crossing, preserving fault honesty."""
    ordinary = []
    build_timeouts = []
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        (build_timeouts if check_case_needs_build_timeout_fixture(axes) else ordinary).append(
            record
        )

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["selection"] == "open":
                # Design note: one fresh declaration per case prevents a solved
                # case from changing the proof selected by a later matrix point.
                events.append(check_declare(record, axes))
            value = str(axes["commands"])
            args = check_args(value, str(axes["extra"]))
            if value == "solved":
                args["commands"] = CHECK_SHAPES[str(axes["proof_shape"])]["solved"]
            events.append(command("check", args, check_expected(record, axes)))

    write_materialized_family("check", ordinary, append)
    materialize_check_build_timeouts(build_timeouts)

def check_publication_is_materialized(axes: dict[str, object]) -> bool:
    return axes["outcome"] == "invalid_configuration" or axes["outcome"] in {
        "open",
        "completed",
        "proof_timeout",
        "pending_build_timeout",
        "rejected_unfinished_dependency",
        "declaration_changed",
        "rejected_axiom_out_of_scope",
    }

def materialize_check_publication(records: list[dict[str, object]]) -> None:
    """Write publication outcomes requiring no external fault injection."""
    selected = []
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if axes["outcome"] in {"open", "completed", "proof_timeout"}:
            selected.append(record)

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        axes0 = members[0]["axes"]
        assert isinstance(axes0, dict)
        layout = str(axes0["layout"])
        library = "Matrix.Main" if layout == "coqproject" else "MatrixDune.Main"
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            name = f"publication_{record['case']}"
            kind = str(axes["declaration_kind"])
            state = {
                "theorem": f"{library}.{name}",
                "statement": f"{kind} {name} : True",
                "status": "Open",
                "goals": OPEN_TRUE["goals"],
            }
            events.append(
                command(
                    "declare",
                    {"name": name, "statement": "True", "kind": kind},
                    state,
                )
            )
            outcome = str(axes["outcome"])
            if outcome == "open":
                commands = "idtac."
                expected = {"state": state, "error": None}
            elif outcome == "completed":
                commands = "exact I."
                expected = {
                    "state": {**state, "status": "Completed", "goals": ""},
                    "error": None,
                }
            else:
                commands = "let rec loop n := loop n in loop 0."
                expected = {
                    "state": state,
                    "error": {
                        "kind": "proof_timeout",
                        "message": "PET operation timed out",
                    },
                }
            events.append(command("check", {"commands": commands}, expected))

    write_materialized_family("check_publication", selected, append)
    materialize_check_publication_timeouts(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["outcome"] == "pending_build_timeout"
        ]
    )
    materialize_check_publication_rejections(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["outcome"] == "rejected_unfinished_dependency"
        ]
    )
    materialize_check_publication_configuration_failures([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["outcome"] == "invalid_configuration"
    ])
    materialize_check_publication_declaration_changes([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["outcome"] == "declaration_changed"
    ])
    materialize_check_publication_axiom_rejections([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["outcome"] == "rejected_axiom_out_of_scope"
    ])

def materialize_check_publication_axiom_rejections(records: list[dict[str, object]]) -> None:
    """Reject each proof kind when a new axiom appears after baseline capture."""
    directory = ROOT / "traces/axiom_injection/generated/check_publication"
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
        layout = str(axes["layout"])
        lifecycle = str(axes["lifecycle"])
        project = "project_coq" if layout == "coqproject" else "project_dune"
        library = "AxiomPub.Main" if layout == "coqproject" else "AxiomPubDune.Main"
        catalog = {"declarations": [
            {"name": f"{library}.witness", "statement": "Definition witness : True := I", "status": "Completed"},
            {"name": f"{library}.seed", "statement": "Theorem seed : True", "status": "Completed"},
        ]}
        start = command("start", {"project_path": project}, catalog)
        events = [event("server_start"), event("user_connect", user="alice"), start]
        if lifecycle == "reconnect":
            events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice"), start])
        elif lifecycle == "restart":
            events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice"), start])
        for record in members:
            case_axes = record["axes"]
            assert isinstance(case_axes, dict)
            kind = str(case_axes["declaration_kind"])
            name = f"axiom_{record['case']}"
            opened = {
                "theorem": f"{library}.{name}",
                "statement": f"{kind} {name} : True",
                "status": "Open",
                "goals": OPEN_TRUE["goals"],
            }
            events.extend([
                command("declare", {"name": name, "kind": kind, "statement": "True"}, opened),
                command("check", {"commands": "exact witness."}, {
                    "state": {**opened, "status": "Rejected", "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n"},
                    "error": {
                        "kind": "axiom_dependency_out_of_scope",
                        "message": "axiom dependency is outside the authorized baseline: witness",
                    },
                }),
            ])
            record["implementation"] = "implemented"
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        target = ROOT / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))

def materialize_check_publication_declaration_changes(records: list[dict[str, object]]) -> None:
    """Group all three declaration kinds per layout/lifecycle after source mutation."""
    directory = ROOT / "traces/declaration_change/generated/check_publication"
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
        layout = str(axes["layout"])
        lifecycle = str(axes["lifecycle"])
        project = "project_coq" if layout == "coqproject" else "project_dune"
        prefix = "DeclChange" if layout == "coqproject" else "DeclChangeDune"
        names = (("A", "t", "Theorem"), ("B", "l", "Lemma"), ("C", "d", "Definition"))
        catalog = {"declarations": [
            {"name": f"{prefix}.{module}.{name}", "statement": f"{kind} {name} : True", "status": "Open"}
            for module, name, kind in names
        ]}
        start = command("start", {"project_path": project}, catalog)
        events = [event("server_start"), event("user_connect", user="alice"), start]
        if lifecycle == "reconnect":
            events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice"), start])
        elif lifecycle == "restart":
            events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice"), start])
        for record in members:
            case_axes = record["axes"]
            assert isinstance(case_axes, dict)
            kind = str(case_axes["declaration_kind"])
            module, name, _ = next(item for item in names if item[2] == kind)
            opened = {
                "theorem": f"{prefix}.{module}.{name}",
                "statement": f"{kind} {name} : True",
                "status": "Open",
                "goals": OPEN_TRUE["goals"],
            }
            events.extend([
                command("prove", {"theorem": name}, opened),
                command("check", {"commands": "exact I."}, {
                    "kind": "declaration_changed",
                    "message": "invalid PET request: declaration interface changed",
                }),
            ])
            record["implementation"] = "implemented"
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        target = ROOT / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))

def materialize_check_publication_configuration_failures(records: list[dict[str, object]]) -> None:
    """Check an open proof after its selected project's config becomes invalid."""
    directory = ROOT / "traces/source_change/generated/check_publication"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    catalog = {"declarations": [
        {"name": "Demo.Main.truth", "statement": "Theorem truth : True", "status": "Open"}
    ]}
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        kind = str(axes["declaration_kind"])
        layout = str(axes["layout"])
        lifecycle = str(axes["lifecycle"])
        project = "project_config" if layout == "coqproject" else "project_config_dune"
        failure = {
            "kind": "invalid_configuration",
            "message": "invalid PET request: " + (
                "_CoqProject load path is incomplete"
                if layout == "coqproject" else "unterminated project expression"
            ),
        }
        name = f"publication_{record['case']}"
        opened = {
            "theorem": f"Demo.Main.{name}", "statement": f"{kind} {name} : True",
            "status": "Open", "goals": OPEN_TRUE["goals"],
        }
        events = [event("server_start"), event("user_connect", user="alice")]
        if lifecycle != "connected":
            events.append(command("start", {"project_path": project}, catalog))
            if lifecycle == "reconnect":
                events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
            else:
                events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
        events.extend([
            command("start", {"project_path": project}, catalog),
            command("declare", {"name": name, "kind": kind, "statement": "True"}, opened),
            command("check", {"commands": "exact I."}, failure),
            event("user_disconnect", user="alice"), event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
        record["implementation"] = "implemented"

def rejected_publication_catalog(layout: str) -> dict[str, object]:
    library = "RejectPub.Main" if layout == "coqproject" else "RejectPubDune.Main"
    return {
        "declarations": [
            {
                "name": f"{library}.unfinished",
                "statement": "Theorem unfinished : True",
                "status": "Open",
            },
            {
                "name": f"{library}.done_true",
                "statement": "Theorem done_true : True",
                "status": "Completed",
            },
        ]
    }

def rejected_publication_prefix(layout: str, lifecycle: str) -> list[dict[str, object]]:
    project = "matrix_project" if layout == "coqproject" else "matrix_dune_project"
    start = command(
        "start", {"project_path": project}, rejected_publication_catalog(layout)
    )
    events = [event("server_start"), event("user_connect", user="alice"), start]
    if lifecycle == "reconnect":
        events.extend(
            [
                event("user_disconnect", user="alice"),
                event("user_connect", user="alice"),
                start,
            ]
        )
    elif lifecycle == "restart":
        events.extend(
            [
                event("server_kill"),
                event("server_start"),
                event("user_connect", user="alice"),
                start,
            ]
        )
    return events

def materialize_check_publication_rejections(
    records: list[dict[str, object]],
) -> None:
    """Reject solved proofs that depend on an unfinished local declaration."""
    directory = ROOT / "traces/publication_rejected/generated/check_publication"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    grouped: dict[str, list[dict[str, object]]] = {}
    for record in records:
        grouped.setdefault(str(record["trace"]), []).append(record)
    for relative, members in grouped.items():
        members.sort(key=lambda item: int(item["case_index"]))
        axes0 = members[0]["axes"]
        assert isinstance(axes0, dict)
        layout = str(axes0["layout"])
        library = "RejectPub.Main" if layout == "coqproject" else "RejectPubDune.Main"
        events = rejected_publication_prefix(layout, str(axes0["lifecycle"]))
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            kind = str(axes["declaration_kind"])
            name = f"rejected_{record['case']}"
            open_state = {
                "theorem": f"{library}.{name}",
                "statement": f"{kind} {name} : True",
                "status": "Open",
                "goals": OPEN_TRUE["goals"],
            }
            events.append(
                command(
                    "declare",
                    {"name": name, "statement": "True", "kind": kind},
                    open_state,
                )
            )
            events.append(
                command(
                    "check",
                    {"commands": "exact unfinished."},
                    {
                        "state": {
                            **open_state,
                            "status": "Rejected",
                            "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n",
                        },
                        "error": {
                            "kind": "unfinished_dependency",
                            "message": (
                                "proof depends on unfinished local declaration 'unfinished'"
                            ),
                        },
                    },
                )
            )
            record["implementation"] = "implemented"
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        (ROOT / relative).write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )

def materialize_check_publication_timeouts(
    records: list[dict[str, object]],
) -> None:
    """Write one fresh build-fault project for each publication timeout case."""
    directory = ROOT / "traces/check_timeout/generated/check_publication"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        timeout_axes = {
            **axes,
            "proof_shape": "True",
        }
        events = check_timeout_prefix(timeout_axes)
        events.append(
            command(
                "prove",
                {"theorem": timeout_target(timeout_axes)},
                timeout_proof_state(timeout_axes),
            )
        )
        events.append(
            command(
                "check",
                {"commands": "exact I."},
                {
                    "state": timeout_proof_state(timeout_axes, "Pending"),
                    "error": {
                        "kind": "build_timeout",
                        "message": (
                            "dependency analysis timed out"
                            if axes["layout"] == "coqproject"
                            else "native build timed out"
                        ),
                    },
                },
            )
        )
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

def timeout_target(axes: dict[str, object]) -> str:
    shape = str(axes["proof_shape"]).lower()
    kind = str(axes["declaration_kind"]).lower()
    return f"timeout_{shape}_{kind}"

def check_timeout_catalog(layout: str) -> dict[str, object]:
    library = "Matrix.Main" if layout == "coqproject" else "MatrixDune.Main"
    declarations = []
    for shape_name, shape in CHECK_SHAPES.items():
        for kind in ("Theorem", "Lemma", "Definition"):
            axes = {"proof_shape": shape_name, "declaration_kind": kind}
            name = timeout_target(axes)
            declarations.append(
                {
                    "name": f"{library}.{name}",
                    "statement": f"{kind} {name} : {shape['statement']}",
                    "status": "Open",
                }
            )
    declarations.append(
        {
            "name": f"{library}.done_true",
            "statement": "Theorem done_true : True",
            "status": "Completed",
        }
    )
    return {"declarations": declarations}

def timeout_proof_state(axes: dict[str, object], status: str = "Open") -> dict[str, object]:
    layout = str(axes["layout"])
    library = "Matrix.Main" if layout == "coqproject" else "MatrixDune.Main"
    shape = CHECK_SHAPES[str(axes["proof_shape"])]
    name = timeout_target(axes)
    return {
        "theorem": f"{library}.{name}",
        "statement": f"{axes['declaration_kind']} {name} : {shape['statement']}",
        "status": status,
        "goals": (
            shape["goals"]
            if status == "Open"
            else "focused:\nunfocused:\nshelved:\ngiven_up:\n"
        ),
    }

def check_timeout_prefix(axes: dict[str, object]) -> list[dict[str, object]]:
    layout = str(axes["layout"])
    lifecycle = str(axes["lifecycle"])
    project = "matrix_project" if layout == "coqproject" else "matrix_dune_project"
    start = command("start", {"project_path": project}, check_timeout_catalog(layout))
    events = [event("server_start"), event("user_connect", user="alice"), start]
    if lifecycle == "reconnect":
        events.extend(
            [
                event("user_disconnect", user="alice"),
                event("user_connect", user="alice"),
                start,
            ]
        )
    elif lifecycle == "restart":
        events.extend(
            [
                event("server_kill"),
                event("server_start"),
                event("user_connect", user="alice"),
                start,
            ]
        )
    return events

def materialize_check_build_timeouts(records: list[dict[str, object]]) -> None:
    """Give every real build-timeout crossing its own process fault boundary."""
    directory = ROOT / "traces/check_timeout/generated/check_build_timeout"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        shape = CHECK_SHAPES[str(axes["proof_shape"])]
        events = check_timeout_prefix(axes)
        events.append(
            command(
                "prove",
                {"theorem": timeout_target(axes)},
                timeout_proof_state(axes),
            )
        )
        events.append(
            command(
                "check",
                {"commands": shape["solved"]},
                {
                    "state": timeout_proof_state(axes, "Pending"),
                    "error": {
                        "kind": "build_timeout",
                        "message": (
                            "dependency analysis timed out"
                            if axes["layout"] == "coqproject"
                            else "native build timed out"
                        ),
                    },
                },
            )
        )
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        target = ROOT / str(record["trace"])
        target.write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )
        record["implementation"] = "implemented"
