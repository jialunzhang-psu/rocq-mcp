"""Search and expression-query scenarios."""

from __future__ import annotations

import shutil

from .common import (
    ATTACHED,
    OPEN_TRUE,
    PROOF_OPEN,
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
from .prove import prove_lifecycle_prefix


def search_pattern_value(value: str) -> object:
    values = {
        "true": "True",
        "equality": "(?x = ?x)",
        "unknown": "rocq_mcp_missing_reference",
        "unicode": "不存在",
        "at_limit": "x" * (1024 * 1024),
        "over_limit": "x" * (1024 * 1024 + 1),
        "multiple_sentences": "True. True",
        "nul": "True\0",
    }
    return values[value]

def search_at_value(value: str) -> object:
    if value == "open_true":
        return declaration("Demo.Main.open_true")
    # Invalid context axes exercise the DeclarationId object boundary.
    return None

def search_args(axes: dict[str, object]) -> dict[str, object]:
    args = {"kind": "search"}
    pattern = str(axes["pattern"])
    if pattern != "missing":
        args["pattern"] = search_pattern_value(pattern)
    at = str(axes["at"])
    if at != "missing":
        args["at"] = search_at_value(at)
    return args

def search_expected(axes: dict[str, object]) -> object:
    if axes["selection"] == "no_project":
        return invalid_request("call start first")
    if axes["pattern"] == "missing":
        return invalid_request("pattern must be a non-empty string")
    if axes["selection"] == "open" and axes["at"] == "open_true":
        return invalid_request(
            "query cannot combine the selected proof with an explicit declaration context"
        )
    if axes["at"] == "missing" and axes["selection"] not in {"open"}:
        return invalid_request("query requires a selected proof or an explicit 'at' declaration")
    if axes["at"] in {"empty", "over_limit"}:
        return invalid_request("query target is invalid")
    if axes["pattern"] in {"multiple_sentences", "nul", "over_limit"}:
        return invalid_request("query expression is invalid")
    if axes["pattern"] == "unknown":
        return {
            "$pet_error_prefix": "PET rejected request (-32003): Coq: The reference rocq_mcp_missing_reference",
        }
    if axes["pattern"] == "unicode":
        return {"$pet_error_prefix": "PET rejected request (-32003): Coq: The reference 不存在"}
    # Search inventories are PET/version dependent.  Keep a semantic anchor
    # instead of asserting an empty (and therefore vacuous) text response or
    # freezing every pretty-printed theorem in the fixture.
    if axes["pattern"] == "true":
        return {"$pet_search": {"contains": ["I: True"]}}
    if axes["pattern"] == "equality":
        return {"$pet_search": {"contains": ["eq_refl:"]}}
    return {"text": ""}

def materialize_search_valid(records: list[dict[str, object]]) -> None:
    def append(events, members):
        for record in members:
            axes = record["axes"]
            events.append(command("query", search_args(axes), search_expected(axes)))
    write_materialized_family("search_valid", records, append)

def search_invalid_args(axes: dict[str, object]) -> dict[str, object]:
    args = {"kind": "search"}
    pattern = str(axes["pattern"])
    if pattern != "missing":
        args["pattern"] = {"wrong_type": 0, "empty": "", "multiple_sentences": "True. True", "nul": "True\0"}[pattern]
    at = str(axes["at"])
    if at != "missing":
        args["at"] = None
    if axes["extra"] == "present":
        args["extra"] = True
    return args

def search_invalid_expected(axes: dict[str, object]) -> object:
    if axes["selection"] == "no_project":
        return invalid_request("call start first")
    if axes["extra"] == "present":
        return invalid_request("unexpected field 'extra'")
    if axes["pattern"] in {"missing", "wrong_type", "empty"}:
        return invalid_request("pattern must be a non-empty string")
    if axes["at"] != "missing":
        return invalid_request("declaration must be an object")
    if axes["pattern"] == "nul":
        return invalid_request("query expression is invalid")
    # The wrapper requires a semantic PET context before it can send a Search
    # request.  Context validation therefore precedes PET syntax validation
    # when the caller selected only a project (or no live proof).
    if axes["selection"] != "open":
        return invalid_request("query requires a selected proof or an explicit 'at' declaration")
    if axes["pattern"] == "multiple_sentences":
        if axes["selection"] == "open":
            return {
                "kind": "proof_step_failed",
                "message": "PET rejected request (-32003): Coq: Tactic expected.",
            }
        return invalid_request("query expression is invalid")
    return invalid_request("declaration 'bad-name' was not found")

def materialize_search_invalid(records: list[dict[str, object]]) -> None:
    def append(events, members):
        for record in members:
            axes = record["axes"]
            events.append(command("query", search_invalid_args(axes), search_invalid_expected(axes)))
    write_materialized_family("search_invalid", records, append)

def search_boundary_args(axes: dict[str, object]) -> dict[str, object]:
    args = {"kind": "search"}
    pattern = str(axes["pattern"])
    if pattern != "missing":
        args["pattern"] = search_pattern_value(pattern)
    at = str(axes["at"])
    if at != "missing":
        args["at"] = search_at_value(at)
    if axes["extra"] == "present":
        args["extra"] = True
    return args

def search_boundary_expected(axes: dict[str, object]) -> object:
    if axes["selection"] == "no_project":
        return invalid_request("call start first")
    if axes["extra"] == "present":
        return invalid_request("unexpected field 'extra'")
    # The adapter validates the required Search pattern before decoding the
    # optional declaration context.  Keep malformed/missing patterns from
    # being masked by an unrelated `at: null` boundary.
    if axes["pattern"] == "missing":
        return invalid_request("pattern must be a non-empty string")
    if axes["at"] != "missing":
        return invalid_request("declaration must be an object")
    if axes["pattern"] == "over_limit":
        return invalid_request("query expression is invalid")
    if axes["pattern"] == "at_limit" and (
        axes["selection"] == "open" or axes["at"] != "missing"
    ):
        return {"$pet_error_prefix": "PET rejected request (-32003): Coq: The reference"}
    return search_expected({**axes, "pattern": "missing" if axes["pattern"] == "missing" else axes["pattern"]})

def materialize_search_boundaries(records: list[dict[str, object]]) -> None:
    def append(events, members):
        for record in members:
            axes = record["axes"]
            events.append(command("query", search_boundary_args(axes), search_boundary_expected(axes)))
    write_materialized_family("search_boundaries", records, append)

TYPE_NAT = {"text": "nat\n     : Set"}

NOTATIONS_NONE = {"text": '{"feedback":[],"proof_finished":false,"st":[]}'}

TYPE_ADD = {"text": "1 + 2\n     : nat"}

NOTATIONS_ADD = {
    "text": (
        '{"feedback":[],"proof_finished":false,"st":[{"locations":'
        '[{"bol_pos":0,"bol_pos_last":0,"bp":38,"ep":41,"fname":'
        '["ToplevelInput"],"line_nb":-1,"line_nb_last":-1}],"notation":'
        '"_ + _","path":"Corelib.Init.Datatypes","scope":"type_scope",'
        '"secpath":"<>"}]}'
    )
}

def query_expression_text(kind: str, expression: str) -> object:
    values: dict[str, object] = {
        "null": None,
        "wrong_type": 0,
        "empty": "",
        "valid": "Nat.add 1 2" if kind == "type" else "1 + 2",
        # The comment makes the Unicode byte sequence part of the Rocq input
        # without changing the queried term or its deterministic result.
        "unicode": "nat (* λ *)",
        "nul": "\0",
        "multiple_sentences": "nat. nat.",
        "unterminated_comment": "(*",
        "at_limit": "nat" + " " * (1024 * 1024 - 3),
        "over_limit": "nat" + " " * (1024 * 1024 - 2),
    }
    return values[expression]

def query_expression_args(
    kind: str, expression: str, extra: str, selection: str
) -> dict[str, object]:
    args: dict[str, object] = {"kind": kind}
    if expression != "missing":
        args["expression"] = query_expression_text(kind, expression)
    # A context-free query is intentionally rejected by the wrapper.  The
    # Project and completed states have no selected live attempt (completed
    # proofs deliberately return no cursor), so provide an explicit PET/Dune
    # declaration context. Open states carry the interactive attempt.
    if selection in {"project", "completed"}:
        args["at"] = declaration("Demo.Main.answer")
    if extra == "present":
        args["extra"] = True
    return args

def query_expression_expected(
    kind: str, expression: str, extra: str, selection: str
) -> object:
    if selection == "no_project":
        return invalid_request("call start first")
    if extra == "present":
        return invalid_request("unexpected field 'extra'")
    if expression in {"missing", "null", "wrong_type", "empty"}:
        return invalid_request("expression must be a non-empty string")
    if expression in {"nul", "over_limit"}:
        return invalid_request("query expression is invalid")
    if expression == "multiple_sentences":
        if kind == "notations":
            return NOTATIONS_NONE
        return {
            "kind": "proof_step_failed",
            "message": "PET rejected request (-32003): Coq: Syntax error: ',' or ')' expected after [term level 200] (in [term]).",
        }
    if expression == "unterminated_comment":
        return {
            "kind": "proof_step_failed",
            "message": "PET rejected request (-32003): Coq: Syntax Error: Lexer: Unterminated comment",
        }
    if expression == "valid":
        return TYPE_ADD if kind == "type" else NOTATIONS_ADD
    return TYPE_NAT if kind == "type" else NOTATIONS_NONE

def materialize_query_expression(records: list[dict[str, object]]) -> None:
    """Write expression-query shape, syntax, Unicode and size boundaries."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            kind = str(axes["kind"])
            expression = str(axes["expression"])
            extra = str(axes["extra"])
            selection = str(axes["selection"])
            events.append(
                command(
                    "query",
                    query_expression_args(kind, expression, extra, selection),
                    query_expression_expected(kind, expression, extra, selection),
                )
            )

    write_materialized_family("query_expression", records, append)

TARGET_QUERY_KINDS = {
    "statement",
    "proof",
    "definition",
    "assumptions",
    "dependencies",
}

def query_failure_is_materialized(axes: dict[str, object]) -> bool:
    """Identify query failures backed by generated command or fixture traces."""
    return (
        axes["kind"] in TARGET_QUERY_KINDS
        and axes["failure"] in {"not_found", "ambiguous"}
    ) or (
        axes["kind"] in {"goals", "type", "notations", "assumptions", "dependencies"}
        and axes["failure"] == "proof_timeout"
    ) or axes["failure"] == "invalid_configuration" or (
        axes["failure"] == "declaration_changed"
        and axes["kind"] == "goals"
    )

def materialize_query_failures(records: list[dict[str, object]]) -> None:
    """Write deterministic target-resolution failures for every target query."""
    deterministic = []
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if axes["kind"] in TARGET_QUERY_KINDS and axes["failure"] == "not_found":
            deterministic.append(record)

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            target = "missing"
            expected = {
                "kind": "not_found",
                "message": "declaration 'missing' was not found",
            }
            events.append(
                command(
                    "query",
                    {"kind": axes["kind"], "target": declaration(target)},
                    expected,
                )
            )

    write_materialized_family("query_failures", deterministic, append)
    materialize_query_failure_timeouts(
        [
            record
            for record in records
            if isinstance(record["axes"], dict)
            and record["axes"]["failure"] == "proof_timeout"
            and record["axes"]["kind"] in {"goals", "type", "notations", "assumptions", "dependencies"}
        ]
    )
    materialize_query_configuration_failures([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["failure"] == "invalid_configuration"
    ])
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if axes["failure"] == "declaration_changed" and axes["kind"] == "goals":
            materialize_query_goals_declaration_change(record)

def materialize_query_goals_declaration_change(record: dict[str, object]) -> None:
    """A goals query must not expose stale PET state after source redefinition."""
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
        command("query", {"kind": "goals"}, {
            "kind": "declaration_changed",
            "message": "invalid PET request: declaration interface changed",
        }),
        event("user_disconnect", user="alice"),
        event("server_kill"),
    ])
    target = ROOT / str(record["trace"])
    target.parent.mkdir(parents=True, exist_ok=True)
    write_jsonl(target, events)
    record["implementation"] = "implemented"

def materialize_query_configuration_failures(records: list[dict[str, object]]) -> None:
    """Invalidate a selected project's configuration before each query kind."""
    directory = ROOT / "traces/source_change/generated/query_failures"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    opened = {
        "theorem": "Demo.Main.truth", "statement": "Theorem truth : True",
        "status": "Open", "goals": OPEN_TRUE["goals"],
    }
    error = {"kind": "invalid_configuration", "message": "Dune workspace discovery failed"}
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        kind = str(axes["kind"])
        lifecycle = str(axes["lifecycle"])
        context = declaration("Demo.Main.truth", "theories/Main.v")
        events = [event("server_start"), event("user_connect", user="alice")]
        if lifecycle != "connected":
            events.append(command("start", {"project_path": "project_config_dune"}, ATTACHED))
            if lifecycle == "reconnect":
                events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
            else:
                events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
        events.append(command("start", {"project_path": "project_config_dune"}, ATTACHED))
        if kind == "goals":
            events.append(
                command(
                    "prove",
                    prove_args("Demo.Main.truth", "theories/Main.v"),
                    opened,
                )
            )
            args = {"kind": kind}
        elif kind in TARGET_QUERY_KINDS:
            args = {"kind": kind, "target": context}
        elif kind in {"type", "notations"}:
            # Design note: configuration is the dimension under test, so give
            # contextual PET queries a valid declaration instead of letting
            # context validation mask Dune discovery.
            args = {
                "kind": kind,
                "expression": "True",
                "at": context,
            }
        elif kind == "search":
            args = {
                "kind": kind,
                "pattern": "True",
                "at": context,
            }
        else:
            args = {"kind": kind}
        expected = (
            {"kind": "invalid_configuration", "message": "Dune workspace discovery failed"}
            if kind == "goals" else error
        )
        events.append(command("query", args, expected))
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(target, events)
        record["implementation"] = "implemented"

def materialize_query_failure_timeouts(records: list[dict[str, object]]) -> None:
    """Exercise query-side PET timeout errors for the operations that use PET."""
    directory = ROOT / "traces/pet_timeout/generated/query_failures"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        kind = str(axes["kind"])
        lifecycle = str(axes["lifecycle"])
        selection = "open" if kind == "goals" else "project"
        events = prove_lifecycle_prefix(selection, lifecycle)
        bob_connected = False
        if kind == "goals":
            # With the timeout fixture's one-lane capacity, selecting another
            # existing theorem evicts Alice's idle PET state without editing
            # her source snapshot.  Publishing a synthetic theorem here would
            # correctly produce declaration_changed before PET is reached.
            duplicate = {
                "theorem": "Demo.Main.duplicate",
                "statement": "Theorem duplicate : True",
                "status": "Open",
                "goals": OPEN_TRUE["goals"],
            }
            events.extend([
                event("user_connect", user="bob"),
                user_command(
                    "bob", "start", {"project_path": "prove_project"}, ATTACHED
                ),
                user_command(
                    "bob",
                    "prove",
                    prove_args("Demo.Main.duplicate"),
                    duplicate,
                ),
            ])
            bob_connected = True
            args = {"kind": "goals"}
        elif kind in {"assumptions", "dependencies"}:
            args = {"kind": kind, "target": declaration("Demo.Main.open_true")}
        else:
            # Type and notation queries are contextual.  Give the project-only
            # timeout case an explicit declaration so validation reaches PET;
            # expecting a PET timeout from a context-free request would test
            # an impossible precedence.
            args = {
                "kind": kind,
                "expression": "True",
                "at": declaration("Demo.Main.open_true"),
            }
        events.append(
            command(
                "query",
                args,
                {"kind": "proof_timeout", "message": "PET operation timed out"},
            )
        )
        if bob_connected:
            events.append(event("user_disconnect", user="bob"))
        events.extend(
            [event("user_disconnect", user="alice"), event("server_kill")]
        )
        write_jsonl((ROOT / str(record["trace"])), events)
        record["implementation"] = "implemented"
