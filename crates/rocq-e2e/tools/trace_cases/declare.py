"""Declaration scenarios."""

from __future__ import annotations

from .common import ATTACHED, declaration, prove_args


from .common import OPEN_TRUE, command, event, invalid_request, write_materialized_family, write_jsonl
from .matrix import ROOT
import json
import shutil


def declare_parameter_args(
    record: dict[str, object], axes: dict[str, object]
) -> dict[str, object]:
    # Design note: library and file are required placement identities in the
    # current protocol. Invalid-axis cases vary only the field they exercise.
    args: dict[str, object] = {
        "library": "Matrix.Main",
        "file": "theories/Main.v",
    }
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
    library = "Matrix.Main"
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
                "library": "Matrix.Main",
                "file": "theories/Main.v",
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
            elif statement_class == "multiple_sentences":
                # A declaration argument is sent to PET as one vernacular
                # header.  PET parses the first sentence as the declaration
                # and consequently rejects a second sentence in the same
                # request (rather than silently discarding it).
                expected = {
                    "kind": "proof_step_failed",
                    "message": "PET rejected request (-32003): Coq: Tactic expected.",
                }
            elif statement_class in {"unterminated_comment", "unterminated_string"}:
                expected = {
                    "kind": "proof_step_failed",
                    "message": (
                        "PET rejected request (-32003): Coq: Syntax Error: Lexer: "
                        + ("Unterminated comment" if statement_class == "unterminated_comment" else "Unterminated string")
                    ),
                }
            elif statement_class == "nul":
                expected = {
                    "kind": "proof_step_failed",
                    "message": "PET rejected request (-32003): Coq: Syntax Error: Lexer: Undefined token",
                }
            else:
                normalized = f"{kind} {name} : True"
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
                args = {
                    "name": "truth",
                    "statement": "True",
                    "kind": "Theorem",
                    "library": "Matrix.Main",
                    "file": "theories/Main.v",
                }
                expected = {
                    "kind": "invalid_declaration",
                    "message": "declaration identity is already present",
                }
            elif axes["failure"] == "invalid_module_context":
                args = {
                    "name": "Missing.new_truth",
                    "statement": "True",
                    "kind": "Theorem",
                    "library": "Matrix.Main",
                    "file": "theories/Main.v",
                }
                expected = {
                    "kind": "invalid_declaration",
                    "message": "declaration name must be local or qualified by its library and modules",
                }
            else:
                args = {
                    "name": "fresh",
                    "statement": "True",
                    "kind": "Theorem",
                    "library": "Missing.Main",
                    "file": "theories/Main.v",
                }
                expected = {
                    "kind": "invalid_configuration",
                    "message": "logical library is not selected by Dune",
                }
            events.append(command("declare", args, expected))

    write_materialized_family("declare_failures", deterministic, append)
    materialize_declare_race_failures([
        record for record in records
        if isinstance(record["axes"], dict)
        and record["axes"]["failure"] == "declaration_changed"
    ])

def materialize_dune_duplicate_theory(records: list[dict[str, object]]) -> None:
    """Exercise Dune's duplicate-theory rejection after lazy attachment.

    ``start`` intentionally asks Dune only for the workspace root; it does not
    load the Rocq rules (or PET) and therefore cannot observe a duplicate
    theory stanza.  The first Dune-owned operation, ``list_files``, loads the
    selected rules and is the operation that must report this configuration
    error.  Keeping the case at that boundary prevents the oracle from
    smuggling eager project indexing back into the protocol.
    """
    directory = ROOT / "traces/ambiguous_library/generated/dune_duplicate_theory"
    if directory.exists():
        shutil.rmtree(directory)
    directory.mkdir(parents=True)
    for record in records:
        axes = record["axes"]
        assert isinstance(axes, dict)
        failure = {
            "kind": "invalid_configuration",
            "message": "Dune rules failed: Error: Rocq theory Demo is defined twice:\n- theory Demo in a/dune:1\n- theory Demo in b/dune:1",
        }
        start = command("start", {"project_path": "project_dune"}, ATTACHED)
        rules = command("list_files", {}, failure)
        events = [
            event("server_start"),
            event("user_connect", user="alice"),
            start,
            rules,
        ]
        if axes["lifecycle"] == "reconnect":
            events.extend([
                event("user_disconnect", user="alice"),
                event("user_connect", user="alice"),
                start,
                rules,
            ])
        elif axes["lifecycle"] == "restart":
            events.extend([
                event("server_kill"),
                event("server_start"),
                event("user_connect", user="alice"),
                start,
                rules,
            ])
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        target = ROOT / str(record["trace"])
        write_jsonl(target, events)
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
        project = "project_dune"
        library = "Race.Main"
        start = command("start", {"project_path": project}, ATTACHED)
        events = [event("server_start"), event("user_connect", user="alice"), start]
        if lifecycle == "reconnect":
            events.extend([event("user_disconnect", user="alice"), event("user_connect", user="alice"), start])
        elif lifecycle == "restart":
            events.extend([event("server_kill"), event("server_start"), event("user_connect", user="alice"), start])
        events.extend([
            command("declare", {
                "name": "fresh",
                "statement": "True",
                "kind": "Theorem",
                "library": library,
                "file": "theories/Main.v",
            }, {
                "kind": "declaration_changed", "message": "invalid PET request: declaration interface changed"
            }),
            event("user_disconnect", user="alice"),
            event("server_kill"),
        ])
        target = ROOT / str(record["trace"])
        write_jsonl(target, events)
        record["implementation"] = "implemented"
