"""Search and expression-query scenarios."""

from __future__ import annotations

import json
import shutil

from .matrix import ROOT
from .common import BASIC_CATALOG, OPEN_TRUE, PROOF_OPEN, command, event, invalid_request, write_materialized_family
from .prove import append_pet_detacher, prove_lifecycle_prefix


def search_arg_value(axis: str, value: str) -> object:
    """Translate one valid-search equivalence class into its JSON value."""
    values: dict[str, dict[str, object]] = {
        "name": {
            "Main": "Main",
            "duplicate": "duplicate",
            "done_true": "done_true",
            "missing_name": "does_not_exist",
        },
        "statement": {"True": "True", "nat": "nat"},
        "status": {
            "Open": "Open",
            "Completed": "Completed",
            "Pending": "Pending",
            "Rejected": "Rejected",
        },
        "offset": {"zero": 0, "one": 1},
        "limit": {"one": 1, "twenty": 20},
    }
    return values[axis][value]

def search_valid_args(axes: dict[str, object]) -> dict[str, object]:
    args: dict[str, object] = {"kind": "search"}
    for axis, field in (
        ("name", "name_contains"),
        ("statement", "statement_pattern"),
        ("status", "status"),
        ("offset", "offset"),
        ("limit", "limit"),
    ):
        value = str(axes[axis])
        if value != "missing":
            args[field] = search_arg_value(axis, value)
    return args

def search_valid_expected(axes: dict[str, object]) -> object:
    if axes["selection"] == "no_project":
        return invalid_request("call start first")
    declarations = BASIC_CATALOG["declarations"]
    selected = list(declarations)
    name = str(axes["name"])
    if name != "missing":
        needle = str(search_arg_value("name", name))
        selected = [item for item in selected if needle in str(item["name"])]
    statement = str(axes["statement"])
    if statement != "missing":
        needle = str(search_arg_value("statement", statement))
        selected = [item for item in selected if needle in str(item["statement"])]
    status = str(axes["status"])
    if status != "missing":
        selected = [item for item in selected if item["status"] == status]
    offset = int(search_arg_value("offset", str(axes["offset"])))
    limit = int(search_arg_value("limit", str(axes["limit"])))
    selected = selected[offset : offset + limit]
    return {"text": "\n".join(str(item["name"]) for item in selected)}

def materialize_search_valid(records: list[dict[str, object]]) -> None:
    """Write the valid search-filter and pagination product."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            events.append(
                command("query", search_valid_args(axes), search_valid_expected(axes))
            )

    write_materialized_family("search_valid", records, append)

def search_invalid_value(axis: str, value: str) -> object:
    """Return a representative for an invalid search argument class."""
    values: dict[str, dict[str, object]] = {
        "name": {
            "wrong_type": 0,
            "empty": "",
            "invalid": "bad-name",
        },
        "statement": {
            "wrong_type": 0,
            "empty": "",
            "invalid": "\0",
        },
        "status": {
            "wrong_type": 0,
            "empty": "",
            "invalid": "Unknown",
        },
        "offset": {
            "wrong_type": "0",
            "negative": -1,
            "fractional": 1.5,
        },
        "limit": {
            "wrong_type": "1",
            "zero": 0,
            "over_max": 101,
            "negative": -1,
        },
    }
    return values[axis][value]

def search_invalid_args(axes: dict[str, object]) -> dict[str, object]:
    args: dict[str, object] = {"kind": "search"}
    for axis, field in (
        ("name", "name_contains"),
        ("statement", "statement_pattern"),
        ("status", "status"),
        ("offset", "offset"),
        ("limit", "limit"),
    ):
        value = str(axes[axis])
        if value != "missing":
            args[field] = search_invalid_value(axis, value)
    if axes["extra"] == "present":
        args["extra"] = True
    return args

def search_invalid_expected(axes: dict[str, object]) -> object:
    """Apply the public adapter's documented validation precedence."""
    if axes["selection"] == "no_project":
        return invalid_request("call start first")
    if axes["extra"] == "present":
        return invalid_request("unexpected field 'extra'")
    if axes["name"] in {"wrong_type", "empty"}:
        return invalid_request("name_contains must be a non-empty string")
    if axes["statement"] in {"wrong_type", "empty"}:
        return invalid_request("statement_pattern must be a non-empty string")
    if axes["status"] != "missing":
        return invalid_request("invalid search status")
    if axes["offset"] != "missing":
        return invalid_request(
            "offset must be an integer from 0 to 18446744073709551615"
        )
    if axes["limit"] != "missing":
        return invalid_request("limit must be an integer from 1 to 100")
    if axes["name"] == "invalid":
        return invalid_request("query name is invalid")
    if axes["statement"] == "invalid":
        return invalid_request("query expression is invalid")
    return {"text": "\n".join(str(item["name"]) for item in BASIC_CATALOG["declarations"])}

def materialize_search_invalid(records: list[dict[str, object]]) -> None:
    """Write all malformed search-field crossings and precedence outcomes."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            events.append(
                command(
                    "query", search_invalid_args(axes), search_invalid_expected(axes)
                )
            )

    write_materialized_family("search_invalid", records, append)

def search_boundary_value(axis: str, value: str) -> object:
    values: dict[str, dict[str, object]] = {
        "name": {
            "unicode": "不存在",
            "at_limit": "a" * (64 * 1024),
            "over_limit": "a" * (64 * 1024 + 1),
        },
        "statement": {
            "unicode": "不存在",
            "at_limit": "True" + " " * (1024 * 1024 - 4),
            "over_limit": "True" + " " * (1024 * 1024 - 3),
        },
        "status": {
            "exact": "Open",
            "lowercase": "open",
            "unknown": "Unknown",
        },
        "offset": {
            "zero": 0,
            "one": 1,
            "negative": -1,
            "fractional": 1.5,
            "string": "1",
            "uint_max": 18_446_744_073_709_551_615,
        },
        "limit": {
            "one": 1,
            "twenty": 20,
            "hundred": 100,
            "zero": 0,
            "over_max": 101,
            "fractional": 1.5,
            "string": "1",
        },
    }
    return values[axis][value]

def search_boundary_args(axes: dict[str, object]) -> dict[str, object]:
    args: dict[str, object] = {"kind": "search"}
    for axis, field in (
        ("name", "name_contains"),
        ("statement", "statement_pattern"),
        ("status", "status"),
        ("offset", "offset"),
        ("limit", "limit"),
    ):
        value = str(axes[axis])
        if value != "missing":
            args[field] = search_boundary_value(axis, value)
    if axes["extra"] == "present":
        args["extra"] = True
    return args

def search_boundary_expected(axes: dict[str, object]) -> object:
    if axes["selection"] == "no_project":
        return invalid_request("call start first")
    if axes["extra"] == "present":
        return invalid_request("unexpected field 'extra'")
    if axes["status"] in {"lowercase", "unknown"}:
        return invalid_request("invalid search status")
    if axes["offset"] in {"negative", "fractional", "string"}:
        return invalid_request(
            "offset must be an integer from 0 to 18446744073709551615"
        )
    if axes["limit"] in {"zero", "over_max", "fractional", "string"}:
        return invalid_request("limit must be an integer from 1 to 100")
    if axes["name"] == "over_limit":
        return invalid_request("query name is invalid")
    if axes["statement"] == "over_limit":
        return invalid_request("query expression is invalid")
    declarations = list(BASIC_CATALOG["declarations"])
    name = str(axes["name"])
    if name != "missing":
        needle = str(search_boundary_value("name", name))
        declarations = [item for item in declarations if needle in str(item["name"])]
    statement = str(axes["statement"])
    if statement != "missing":
        needle = str(search_boundary_value("statement", statement))
        declarations = [
            item for item in declarations if needle in str(item["statement"])
        ]
    if axes["status"] == "exact":
        declarations = [item for item in declarations if item["status"] == "Open"]
    offset = (
        0
        if axes["offset"] == "missing"
        else int(search_boundary_value("offset", str(axes["offset"])))
    )
    limit = (
        20
        if axes["limit"] == "missing"
        else int(search_boundary_value("limit", str(axes["limit"])))
    )
    declarations = declarations[offset : offset + limit]
    return {"text": "\n".join(str(item["name"]) for item in declarations)}

def materialize_search_boundaries(records: list[dict[str, object]]) -> None:
    """Write Unicode, byte-boundary, enum and numeric search crossings."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            events.append(
                command(
                    "query", search_boundary_args(axes), search_boundary_expected(axes)
                )
            )

    write_materialized_family("search_boundaries", records, append)

TYPE_NAT = {"text": '[3,"nat\\n     : Set"]'}

NOTATIONS_NONE = {"text": '{"feedback":[],"proof_finished":false,"st":[]}'}

TYPE_ADD = {"text": '[3,"1 + 2\\n     : nat"]'}

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

def query_expression_args(kind: str, expression: str, extra: str) -> dict[str, object]:
    args: dict[str, object] = {"kind": kind}
    if expression != "missing":
        args["expression"] = query_expression_text(kind, expression)
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
        return invalid_request("query expression contains multiple vernacular sentences")
    if expression == "unterminated_comment":
        return invalid_request("query expression has unterminated syntax")
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
                    query_expression_args(kind, expression, extra),
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
        if axes["kind"] in TARGET_QUERY_KINDS and axes["failure"] in {
            "not_found",
            "ambiguous",
        }:
            deterministic.append(record)

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            failure = str(axes["failure"])
            target = "missing" if failure == "not_found" else "duplicate"
            expected = (
                {
                    "kind": "not_found",
                    "message": "declaration 'missing' was not found",
                }
                if failure == "not_found"
                else {
                    "kind": "ambiguous",
                    "message": "declaration name is ambiguous",
                }
            )
            events.append(
                command(
                    "query",
                    {"kind": axes["kind"], "target": target},
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
        command("query", {"kind": "goals"}, {
            "kind": "declaration_changed",
            "message": "invalid PET request: declaration interface changed",
        }),
        event("user_disconnect", user="alice"),
        event("server_kill"),
    ])
    target = ROOT / str(record["trace"])
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
    record["implementation"] = "implemented"

def materialize_query_configuration_failures(records: list[dict[str, object]]) -> None:
    """Invalidate a selected project's configuration before each query kind."""
    directory = ROOT / "traces/source_change/generated/query_failures"
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
    error = {"kind": "invalid_configuration", "message": "_CoqProject load path is incomplete"}
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        kind = str(axes["kind"])
        lifecycle = str(axes["lifecycle"])
        events = [event("server_start"), event("user_connect", user="alice")]
        if lifecycle != "connected":
            events.append(command("start", {"project_path": "project_config"}, catalog))
            if lifecycle == "reconnect":
                events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice")])
            else:
                events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice")])
        events.append(command("start", {"project_path": "project_config"}, catalog))
        if kind == "goals":
            events.append(command("prove", {"theorem": "truth"}, opened))
            args = {"kind": kind}
        elif kind in TARGET_QUERY_KINDS:
            args = {"kind": kind, "target": "truth"}
        elif kind in {"type", "notations"}:
            args = {"kind": kind, "expression": "True"}
        else:
            args = {"kind": kind}
        expected = (
            {"kind": "invalid_configuration", "message": "invalid PET request: _CoqProject load path is incomplete"}
            if kind == "goals" else error
        )
        events.append(command("query", args, expected))
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
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
            append_pet_detacher(events, record)
            bob_connected = True
            args = {"kind": "goals"}
        elif kind in {"assumptions", "dependencies"}:
            args = {"kind": kind, "target": "open_true"}
        else:
            args = {"kind": kind, "expression": "True"}
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
        (ROOT / str(record["trace"])).write_text(
            "".join(
                json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n"
                for item in events
            )
        )
        record["implementation"] = "implemented"
