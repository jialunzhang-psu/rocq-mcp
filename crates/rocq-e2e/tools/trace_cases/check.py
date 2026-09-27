"""Check, try, and publication-result scenarios."""

from __future__ import annotations

from .common import ATTACHED, declaration, prove_args

import json
import shutil

from .matrix import ROOT, check_case_needs_build_timeout_fixture
from .common import OPEN_TRUE, PROOF_OPEN, command, event, invalid_request, write_materialized_family, write_jsonl


PROOF_OPEN_AFTER_IDTAC = PROOF_OPEN.copy()
PROOF_OPEN_SOLVED = {
    **PROOF_OPEN,
    "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n",
}


def sized_idtac(length: int) -> str:
    """Return one complete idtac sentence with exactly ``length`` bytes."""
    return 'idtac "' + "a" * (length - 9) + '".'

def try_attempts(value: str) -> object:
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

def try_args(value: str, extra: str) -> dict[str, object]:
    args: dict[str, object] = {}
    if value != "missing":
        args["attempts"] = try_attempts(value)
    if extra == "present":
        args["extra"] = True
    return args

def attempt_result(solved: bool) -> dict[str, object]:
    return {
        "solved": solved,
        "state": PROOF_OPEN_SOLVED if solved else PROOF_OPEN_AFTER_IDTAC,
        "error": None,
    }

def try_parameters_expected(
    value: str, extra: str, selection: str
) -> object:
    if extra == "present":
        return invalid_request("unexpected field 'extra'")
    if selection != "open":
        return invalid_request("call prove first")
    if value in {"missing", "null", "wrong_container"}:
        return invalid_request("attempts must be an array")
    if value == "wrong_item":
        return invalid_request("attempt must be a string")
    if value == "empty_item":
        return invalid_request("attempt 0 is empty")
    if value in {"count_0", "count_21"}:
        return invalid_request("attempt count must be between 1 and 20")
    if value == "over_limit":
        return invalid_request("proof sentence is too large")
    if value == "two_sentences":
        return {"attempts": [attempt_result(True)]}
    if value == "at_limit":
        return {"attempts": [attempt_result(False)]}
    if value == "duplicate":
        return {"attempts": [attempt_result(False), attempt_result(False)]}
    if value == "mixed":
        return {"attempts": [attempt_result(False), attempt_result(True)]}
    count = int(value.removeprefix("count_"))
    return {"attempts": [attempt_result(True) for _ in range(count)]}

def materialize_try_parameters(records: list[dict[str, object]]) -> None:
    """Write attempt-container, count and payload-boundary crossings."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            value = str(axes["attempts"])
            extra = str(axes["extra"])
            selection = str(axes["selection"])
            events.append(
                command(
                    "try",
                    try_args(value, extra),
                    try_parameters_expected(value, extra, selection),
                )
            )

    write_materialized_family("try_parameters", records, append)

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

def materialize_try_order(records: list[dict[str, object]]) -> None:
    """Write all observable attempt-result permutations."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        axes = members[0]["axes"]
        assert isinstance(axes, dict)
        selection = str(axes["selection"])
        if selection == "open":
            events.append(
                command(
                    "prove",
                    prove_args("Matrix.Main.pair", "theories/Main.v"),
                    PAIR_OPEN,
                )
            )
        for record in members:
            case_axes = record["axes"]
            assert isinstance(case_axes, dict)
            order = str(case_axes["order"]).split("_")
            extra = str(case_axes["extra"])
            args: dict[str, object] = {
                "attempts": [ORDER_ITEMS[item][0] for item in order]
            }
            if extra == "present":
                args["extra"] = True
            if extra == "present":
                expected: object = invalid_request("unexpected field 'extra'")
            elif selection != "open":
                expected = invalid_request("call prove first")
            else:
                expected = {"attempts": [ORDER_ITEMS[item][1] for item in order]}
            events.append(command("try", args, expected))

    write_materialized_family("try_order", records, append)

def try_failure_is_materialized(axes: dict[str, object]) -> bool:
    return axes["failure"] in {"proof_timeout", "invalid_configuration", "declaration_changed"}

def materialize_try_failures(records: list[dict[str, object]]) -> None:
    """Write attempt-local PET timeout results; environment faults stay separate."""
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
                    "try",
                    {"attempts": ["let rec loop n := loop n in loop 0."]},
                    {
                        "attempts": [
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

    write_materialized_family("try_failures", selected, append)
    materialize_try_configuration_failures([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["failure"] == "invalid_configuration"
    ])
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if axes["failure"] == "declaration_changed":
            materialize_try_declaration_change(record)

def materialize_try_declaration_change(record: dict[str, object]) -> None:
    """Change the selected theorem header after prove and check attempt replay."""
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
    start = command("start", {"project_path": "project_content"}, ATTACHED)
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
        command("prove", prove_args("Demo.Main.truth"), opened),
        command("try", {"attempts": ["exact I."]}, {
            "kind": "declaration_changed",
            "message": "declaration source changed while proof was open",
        }),
        event("user_disconnect", user="alice"),
        event("server_kill"),
    ])
    target = ROOT / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    write_jsonl(target, events)
    record["implementation"] = "implemented"

def materialize_try_configuration_failures(records: list[dict[str, object]]) -> None:
    """Return an attempt-local error after a selected project's load path breaks."""
    directory = ROOT / "traces/source_change/generated/try_failures"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    opened = {
        "theorem": "Demo.Main.truth", "statement": "Theorem truth : True",
        "status": "Open", "goals": OPEN_TRUE["goals"],
    }
    expected = {
        "kind": "invalid_configuration",
        "message": "Dune workspace discovery failed",
    }
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
            command("prove", prove_args("Demo.Main.truth", "theories/Main.v"), opened),
            command("try", {"attempts": ["idtac."]}, expected),
            event("user_disconnect", user="alice"), event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(target, events)
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
            args: dict[str, object] = {"attempts": [ESCAPING_COMMANDS[str(axes["head"])] ]}
            if axes["extra"] == "present":
                args["extra"] = True
            if axes["extra"] == "present":
                expected: object = invalid_request("unexpected field 'extra'")
            elif axes["selection"] != "open":
                expected = invalid_request("call prove first")
            else:
                state = {
                    "theorem": "Matrix.Main.truth",
                    "statement": "Theorem truth : True",
                    "status": "Open",
                    "goals": OPEN_TRUE["goals"],
                }
                if axes["head"] in ("Focus", "Unfocus"):
                    expected = {"selected": 0, "state": state, "rejected": [], "error": None}
                else:
                    # The request passed transport validation and PET rejected
                    # the sentence as non-proof vernacular. `check` therefore
                    # preserves the ordered-check envelope and current state.
                    if axes["head"] == "Undo_all":
                        error: object = {
                            "$pet_error_prefix": (
                                "PET rejected request (-32003): Coq: Syntax"
                            )
                        }
                    elif axes["head"] == "Drop":
                        error = {
                            "$pet_error_prefix": (
                                "PET rejected request (-32003): Coq: The reference Drop"
                            )
                        }
                    else:
                        error = {
                            "kind": "invalid_request",
                            "message": (
                                "unsafe proof command: PET parsed a proof command "
                                "as global vernacular or a control command"
                            ),
                        }
                    expected = {
                        "selected": None,
                        "state": state,
                        "rejected": [error],
                        "error": None,
                    }
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
        args["attempts"] = None
    elif value == "wrong_type":
        args["attempts"] = 0
    elif value == "empty":
        args["attempts"] = []
    elif value == "whitespace":
        args["attempts"] = ["   "]
    elif value == "missing_dot":
        args["attempts"] = ["idtac"]
    elif value == "unterminated_comment":
        args["attempts"] = ["(*"]
    elif value == "unterminated_string":
        args["attempts"] = ['idtac "unterminated.']
    elif value == "unsolved":
        args["attempts"] = ["idtac."]
    elif value == "given_up":
        args["attempts"] = ["solve [admit]."]
    elif value == "partial_failure":
        # The first sentence succeeds in PET, but the whole fragment is atomic.
        args["attempts"] = ["idtac. fail."]
    elif value == "at_limit":
        args["attempts"] = ['idtac "' + ("a" * (4096 - len('idtac "".'))) + '".']
    elif value == "over_limit":
        args["attempts"] = ['idtac "' + ("a" * (4097 - len('idtac "".'))) + '".']
    elif value == "proof_timeout":
        args["attempts"] = ["let rec loop n := loop n in loop 0."]
    elif value == "build_timeout":
        # Only state-gated/unknown-field variants reach the ordinary corpus.
        args["attempts"] = ["exact I."]
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
    library = "Matrix.Main"
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
            "library": "Matrix.Main",
            "file": "theories/Main.v",
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
    if value in {"missing", "null", "wrong_type"}:
        return invalid_request("attempts must be an array")
    if value == "empty":
        return invalid_request("attempt count must be between 1 and 20")
    if value == "whitespace":
        return invalid_request("attempt 0 is empty")
    if value in {"missing_dot", "unterminated_comment", "unterminated_string"}:
        # PET, rather than the wrapper byte framer, owns incomplete-sentence
        # syntax. Keep the stable transport/error prefix and leave the native
        # lexer wording flexible across PET/Rocq versions. The failed fragment
        # remains inside the ordered check envelope.
        return {
            "selected": None,
            "state": check_state(record, axes, "Open"),
            "rejected": [{
                "$pet_error_prefix": "PET rejected request (-32003): Coq: Syntax"
            }],
            "error": None,
        }
    if value == "over_limit":
        return invalid_request("proof sentence is too large")
    state = check_state(record, axes, "Completed" if value == "solved" else "Open")
    if value == "given_up":
        return {
            "selected": None,
            "state": state,
            "rejected": [{
                "kind": "invalid_request",
                "message": "unsafe proof command: PET reports given-up goals after this command",
            }],
            "error": None,
        }
    if value == "partial_failure":
        return {
            "selected": None,
            "state": state,
            "rejected": [{
                "kind": "proof_step_failed",
                "message": "PET rejected request (-32003): Coq: Tactic failure.",
            }],
            "error": None,
        }
    if value == "proof_timeout":
        return {
            "selected": None,
            "state": state,
            "rejected": [{"kind": "proof_timeout", "message": "PET operation timed out"}],
            "error": None,
        }
    return {"selected": 0, "state": state, "rejected": [], "error": None}

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
                args["attempts"] = [CHECK_SHAPES[str(axes["proof_shape"])]["solved"]]
            events.append(command("check", args, check_expected(record, axes)))

    write_materialized_family("check", ordinary, append)
    materialize_check_build_timeouts(build_timeouts)

def check_publication_is_materialized(axes: dict[str, object]) -> bool:
    return axes["outcome"] == "invalid_configuration" or axes["outcome"] in {
        "open",
        "completed",
        "proof_timeout",
        "build_timeout",
        "unfinished_dependency",
        "declaration_changed",
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
        library = "Matrix.Main"
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
                    {
                        "name": name,
                        "statement": "True",
                        "kind": kind,
                        "library": "Matrix.Main",
                        "file": "theories/Main.v",
                    },
                    state,
                )
            )
            outcome = str(axes["outcome"])
            if outcome == "open":
                commands = "idtac."
                expected = {"selected": 0, "state": state, "rejected": [], "error": None}
            elif outcome == "completed":
                commands = "exact I."
                expected = {
                    "selected": 0,
                    "state": {**state, "status": "Completed", "goals": ""},
                    "rejected": [],
                    "error": None,
                }
            else:
                commands = "let rec loop n := loop n in loop 0."
                expected = {
                    "selected": None,
                    "state": state,
                    "rejected": [{
                        "kind": "proof_timeout",
                        "message": "PET operation timed out",
                    }],
                    "error": None,
                }
            events.append(command("check", {"attempts": [commands]}, expected))

    write_materialized_family("check_publication", selected, append)
    materialize_check_publication_timeouts(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["outcome"] == "build_timeout"
        ]
    )
    materialize_check_publication_rejections(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["outcome"] == "unfinished_dependency"
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
        lifecycle = str(axes["lifecycle"])
        project = "project_dune"
        prefix = "DeclChange"
        names = (("A", "t", "Theorem"), ("B", "l", "Lemma"), ("C", "d", "Definition"))
        start = command("start", {"project_path": project}, ATTACHED)
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
                command(
                    "prove",
                    prove_args(f"{prefix}.{module}.{name}", f"theories/{module}.v"),
                    opened,
                ),
                command("check", {"attempts": ["exact I."]}, {
                    "kind": "declaration_changed",
                    "message": "declaration source changed while proof was open",
                }),
            ])
            record["implementation"] = "implemented"
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        target = ROOT / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(target, events)

def materialize_check_publication_configuration_failures(records: list[dict[str, object]]) -> None:
    """Check an open proof after its selected project's config becomes invalid."""
    directory = ROOT / "traces/source_change/generated/check_publication"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        kind = str(axes["declaration_kind"])
        layout = str(axes["layout"])
        lifecycle = str(axes["lifecycle"])
        project = "project_config_dune"
        name = f"publication_{record['case']}"
        opened = {
            "theorem": f"Demo.Main.{name}", "statement": f"{kind} {name} : True",
            "status": "Open", "goals": OPEN_TRUE["goals"],
        }
        # Attempt/environment validation precedes PET selection. A malformed
        # Dune view is therefore a top-level request failure, not a selected
        # fragment followed by a publication error.
        failure = {
            "kind": "invalid_configuration",
            "message": "Dune workspace discovery failed",
        }
        events = [event("server_start"), event("user_connect", user="alice")]
        if lifecycle != "connected":
            events.append(command("start", {"project_path": project}, ATTACHED))
            if lifecycle == "reconnect":
                events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
            else:
                events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
        events.extend([
            command("start", {"project_path": project}, ATTACHED),
            command(
                "declare",
                {
                    "name": name,
                    "kind": kind,
                    "statement": "True",
                    "library": "Demo.Main",
                    "file": "theories/Main.v",
                },
                opened,
            ),
            command("check", {"attempts": ["exact I."]}, failure),
            event("user_disconnect", user="alice"), event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(target, events)
        record["implementation"] = "implemented"

def rejected_publication_prefix(layout: str, lifecycle: str) -> list[dict[str, object]]:
    project = "matrix_dune_project"
    start = command("start", {"project_path": project}, ATTACHED)
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
        library = "RejectPubDune.Main"
        # Dune does not expose a generated `_RocqProject` for this fixture.
        # PET therefore resolves the source-relative reference through the
        # loaded compilation unit as `Main.unfinished`; the trust audit keeps
        # that PET-resolved path instead of manufacturing the Dune prefix.
        unfinished_name = "Main.unfinished"
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
                    {
                        "name": name,
                        "statement": "True",
                        "kind": kind,
                        "library": library,
                        "file": "theories/Main.v",
                    },
                    open_state,
                )
            )
            events.append(
                command(
                    "check",
                    {"attempts": ["exact unfinished."]},
                    {
                        # PET accepted and selected the tactic; trust rejection
                        # is a post-selection close error, not an alternative
                        # candidate rejection.
                        "selected": 0,
                        "state": {
                            **open_state,
                            "status": "Open",
                            "goals": "focused:\nunfocused:\nshelved:\ngiven_up:\n",
                        },
                        "rejected": [],
                        "error": {
                            "kind": "unfinished_dependency",
                            "message": (
                                "proof depends on unfinished local declaration "
                                f"'{unfinished_name}'"
                            ),
                        },
                    },
                )
            )
            record["implementation"] = "implemented"
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        write_jsonl((ROOT / relative), events)

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
                prove_args(
                    f"Matrix.Main.{timeout_target(timeout_axes)}",
                    "theories/Main.v",
                ),
                timeout_proof_state(timeout_axes),
            )
        )
        events.append(
            command(
                "check",
                {"attempts": ["exact I."]},
                {
                    "selected": 0,
                    "state": timeout_proof_state(timeout_axes, goals_clear=True),
                    "rejected": [],
                    "error": {
                        "kind": "build_timeout",
                        "message": "native build timed out",
                    },
                },
            )
        )
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        write_jsonl((ROOT / str(record["trace"])), events)
        record["implementation"] = "implemented"

def timeout_target(axes: dict[str, object]) -> str:
    shape = str(axes["proof_shape"]).lower()
    kind = str(axes["declaration_kind"]).lower()
    return f"timeout_{shape}_{kind}"

def timeout_proof_state(
    axes: dict[str, object], goals_clear: bool = False
) -> dict[str, object]:
    layout = str(axes["layout"])
    library = "Matrix.Main"
    shape = CHECK_SHAPES[str(axes["proof_shape"])]
    name = timeout_target(axes)
    return {
        "theorem": f"{library}.{name}",
        "statement": f"{axes['declaration_kind']} {name} : {shape['statement']}",
        "status": "Open",
        "goals": (
            "focused:\nunfocused:\nshelved:\ngiven_up:\n"
            if goals_clear
            else shape["goals"]
        ),
    }

def check_timeout_prefix(axes: dict[str, object]) -> list[dict[str, object]]:
    layout = str(axes["layout"])
    lifecycle = str(axes["lifecycle"])
    project = "matrix_dune_project"
    start = command("start", {"project_path": project}, ATTACHED)
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
                prove_args(
                    f"Matrix.Main.{timeout_target(axes)}", "theories/Main.v"
                ),
                timeout_proof_state(axes),
            )
        )
        events.append(
            command(
                "check",
                {"attempts": [shape["solved"]]},
                {
                    "selected": 0,
                    "state": timeout_proof_state(axes, goals_clear=True),
                    "rejected": [],
                    "error": {
                        "kind": "build_timeout",
                        "message": "native build timed out",
                    },
                },
            )
        )
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        target = ROOT / str(record["trace"])
        write_jsonl(target, events)
        record["implementation"] = "implemented"
