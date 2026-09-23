"""Declaration scenarios."""

from __future__ import annotations


from .common import OPEN_TRUE, command, event, invalid_request, proof_lifecycle_prefix, write_materialized_family
from .matrix import ROOT
import json
import shutil


def declare_parameter_args(
    record: dict[str, object], axes: dict[str, object]
) -> dict[str, object]:
    args: dict[str, object] = {}
    name = str(axes["name"])
    if name == "null":
        args["name"] = None
    elif name == "wrong_type":
        args["name"] = 0
    elif name == "empty":
        args["name"] = ""
    elif name == "valid":
        args["name"] = f"fresh_{record['case']}"
    statement = str(axes["statement"])
    if statement == "null":
        args["statement"] = None
    elif statement == "wrong_type":
        args["statement"] = 0
    elif statement == "empty":
        args["statement"] = ""
    elif statement == "valid":
        args["statement"] = "True"
    kind = str(axes["kind"])
    if kind == "null":
        args["kind"] = None
    elif kind == "wrong_type":
        args["kind"] = 0
    elif kind == "empty":
        args["kind"] = ""
    elif kind != "missing":
        args["kind"] = kind
    if axes["extra"] == "present":
        args["extra"] = True
    return args

def declare_parameter_expected(
    record: dict[str, object], axes: dict[str, object]
) -> object:
    if axes["extra"] == "present":
        return invalid_request("unexpected field 'extra'")
    if axes["selection"] == "no_project":
        return invalid_request("call start first")
    if axes["name"] != "valid":
        return invalid_request("name must be a non-empty string")
    kind = str(axes["kind"])
    if kind in {"null", "wrong_type", "empty"}:
        return {
            "kind": "invalid_declaration",
            "message": "declaration kind must be a non-empty string",
        }
    if kind == "Axiom":
        return {
            "kind": "invalid_declaration",
            "message": "unsupported declaration kind 'Axiom'",
        }
    if axes["statement"] != "valid":
        return invalid_request("statement must be a non-empty string")
    keyword = "Theorem" if kind == "missing" else kind
    library = "Matrix.Main" if axes["layout"] == "coqproject" else "MatrixDune.Main"
    name = f"fresh_{record['case']}"
    return {
        "theorem": f"{library}.{name}",
        "statement": f"{keyword} {name} : True",
        "status": "Open",
        "goals": OPEN_TRUE["goals"],
    }

def materialize_declare_parameters(records: list[dict[str, object]]) -> None:
    """Write the full public declare argument-shape product."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            events.append(
                command(
                    "declare",
                    declare_parameter_args(record, axes),
                    declare_parameter_expected(record, axes),
                )
            )

    write_materialized_family("declare_parameters", records, append)

def boundary_declaration_name(record: dict[str, object], value: str) -> str:
    case = str(record["case"])
    if value == "unicode":
        return f"定理_{case}"
    if value == "leading_dot":
        return f".{case}"
    if value == "trailing_dot":
        return f"n{case}."
    if value == "double_dot":
        return f"n..{case}"
    if value == "hyphen":
        return f"n-{case}"
    if value == "slash":
        return f"n/{case}"
    length = 65_536 if value == "at_limit" else 65_537
    prefix = f"n{case}"
    return prefix + "a" * (length - len(prefix))

def boundary_statement(kind: str, name: str, value: str) -> str:
    if value == "body":
        return "True"
    if value == "whitespace":
        return "   "
    if value == "full_header":
        return f"{kind} {name} : True"
    if value == "mismatched_header":
        return f"{kind} other : True"
    if value == "multiple_sentences":
        return "True. False."
    if value == "nul":
        return "True\0"
    if value == "unterminated_comment":
        return "(*"
    if value == "unterminated_string":
        return '"'
    length = 1024 * 1024 if value == "at_limit" else 1024 * 1024 + 1
    return "True" + " " * (length - 4)

def materialize_declare_boundaries(records: list[dict[str, object]]) -> None:
    """Write declaration identity, syntax and byte-boundary crossings."""

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            name_class = str(axes["name"])
            statement_class = str(axes["statement"])
            kind = str(axes["kind"])
            extra = str(axes["extra"])
            name = boundary_declaration_name(record, name_class)
            statement = boundary_statement(kind, name, statement_class)
            args: dict[str, object] = {
                "name": name,
                "statement": statement,
                "kind": kind,
            }
            if extra == "present":
                args["extra"] = True
            if extra == "present":
                expected: object = invalid_request("unexpected field 'extra'")
            elif name_class not in {"unicode", "at_limit"}:
                expected = {
                    "kind": "invalid_declaration",
                    "message": "declaration identity is invalid",
                }
            elif statement_class in {"whitespace", "over_limit"}:
                expected = {
                    "kind": "invalid_declaration",
                    "message": "declaration statement is empty or oversized",
                }
            elif statement_class == "mismatched_header":
                expected = {
                    "kind": "invalid_declaration",
                    "message": "declaration statement identity does not match request",
                }
            elif statement_class in {"unterminated_comment", "unterminated_string"}:
                expected = {
                    "kind": "invalid_configuration",
                    "message": "unterminated Rocq comment or string",
                }
            elif statement_class == "nul":
                expected = {
                    "kind": "proof_step_failed",
                    "message": (
                        "PET rejected request (-32006): Theorem_not_found: "
                        "[find_thm] Theorem not found!"
                    ),
                }
            else:
                normalized = (
                    f"{kind} {name} : True. False."
                    if statement_class == "multiple_sentences"
                    else f"{kind} {name} : True"
                )
                expected = {
                    "theorem": f"Matrix.Main.{name}",
                    "statement": normalized,
                    "status": "Open",
                    "goals": OPEN_TRUE["goals"],
                }
            events.append(command("declare", args, expected))

    write_materialized_family("declare_boundaries", records, append)

def declare_failure_is_materialized(axes: dict[str, object]) -> bool:
    """Identify failure classes needing no external mutation or permission fault."""
    return axes["failure"] in {
        "duplicate",
        "invalid_module_context",
        "logical_library_unavailable",
        "state_directory_unavailable",
    }

def materialize_declare_failures(records: list[dict[str, object]]) -> None:
    """Write deterministic declaration-resolution failures.

    Filesystem, source-race, and load-path faults remain owned by environment
    fixtures rather than being renamed into an ordinary validation error.
    """
    deterministic = []
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        if declare_failure_is_materialized(axes):
            deterministic.append(record)

    def append(events: list[dict[str, object]], members: list[dict[str, object]]) -> None:
        for record in members:
            axes = record["axes"]
            assert isinstance(axes, dict)
            if axes["failure"] == "duplicate":
                args = {"name": "truth", "statement": "True", "kind": "Theorem"}
                expected = {
                    "kind": "invalid_declaration",
                    "message": "declaration identity is already present",
                }
            elif axes["failure"] == "invalid_module_context":
                args = {
                    "name": "Missing.new_truth",
                    "statement": "True",
                    "kind": "Theorem",
                }
                expected = {
                    "kind": "invalid_declaration",
                    "message": "module context does not match declaration identity",
                }
            else:
                empty = (
                    "empty_project"
                    if axes["layout"] == "coqproject"
                    else "empty_dune_project"
                )
                events.append(
                    command("start", {"project_path": empty}, {"declarations": []})
                )
                args = {"name": "fresh", "statement": "True", "kind": "Theorem"}
                expected = {
                    "kind": "invalid_configuration",
                    "message": "project has no logical library",
                }
            events.append(command("declare", args, expected))

    write_materialized_family("declare_failures", deterministic, append)
    materialize_declare_state_failures([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["failure"] == "state_directory_unavailable"
    ])
    materialize_declare_race_failures([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["failure"] == "declaration_changed"
    ])
    materialize_declare_ambiguous_failures([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["failure"] == "ambiguous_location"
    ])

def materialize_declare_ambiguous_failures(records: list[dict[str, object]]) -> None:
    """A new logical library has two equally specific reversible mappings."""
    directory = ROOT / "traces/ambiguous_library/generated/declare_failures"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    catalog = {"declarations": [
        {"name": "Demo.A.a", "statement": "Theorem a : True", "status": "Completed"},
        {"name": "Demo.B.b", "statement": "Theorem b : True", "status": "Completed"},
    ]}
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        project = "project_coq" if axes["layout"] == "coqproject" else "project_dune"
        start = command("start", {"project_path": project}, catalog)
        events = [event("server_start"), event("user_connect", user="alice"), start]
        if axes["lifecycle"] == "reconnect":
            events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice"), start])
        elif axes["lifecycle"] == "restart":
            events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice"), start])
        events.extend([
            command("declare", {"library": "Demo.New", "name": "truth", "statement": "True"}, {
                "kind": "ambiguous", "message": "new logical library has ambiguous load paths",
            }),
            event("user_disconnect", user="alice"), event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
        record["implementation"] = "implemented"

def materialize_declare_race_failures(records: list[dict[str, object]]) -> None:
    """Change disposable source after anchoring but before PET reads it.

    These traces require the test-only ``fault-injection`` server feature.
    The oracle remains the exact public MCP response, not an internal call.
    """
    directory = ROOT / "traces/declare_race/generated/declare_failures"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        layout = str(axes["layout"])
        lifecycle = str(axes["lifecycle"])
        project = "project_coq" if layout == "coqproject" else "project_dune"
        library = "Race.Main" if layout == "coqproject" else "RaceDune.Main"
        start = command("start", {"project_path": project}, {"declarations": [
            {"name": f"{library}.seed", "statement": "Theorem seed : True", "status": "Completed"}
        ]})
        events = [event("server_start"), event("user_connect", user="alice"), start]
        if lifecycle == "reconnect":
            events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice"), start])
        elif lifecycle == "restart":
            events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice"), start])
        events.extend([
            command("declare", {"name": "fresh", "statement": "True", "kind": "Theorem"}, {
                "kind": "declaration_changed", "message": "invalid PET request: declaration interface changed"
            }),
            event("user_disconnect", user="alice"),
            event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
        record["implementation"] = "implemented"

def materialize_declare_state_failures(records: list[dict[str, object]]) -> None:
    """Block PET's disposable workspace after project selection, before declare."""
    directory = ROOT / "traces/proof/generated/declare_state_unavailable"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        layout = str(axes["layout"])
        lifecycle = str(axes["lifecycle"])
        events = proof_lifecycle_prefix("project", lifecycle, layout)
        events.extend([
            command("declare", {"name": "fresh_state", "kind": "Theorem", "statement": "True"}, {
                "kind": "invalid_configuration", "message": "PET workspace unavailable",
            }),
            event("user_disconnect", user="alice"),
            event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("".join(json.dumps(item, ensure_ascii=False, separators=(",", ":")) + "\n" for item in events))
        record["implementation"] = "implemented"

FILES_CATALOG = {
    "declarations": [
        {"name": f"Files.{name.upper()}.{name}", "statement": f"Theorem {name} : True", "status": "Open"}
        for name in ("a", "b", "c")
    ]
}
