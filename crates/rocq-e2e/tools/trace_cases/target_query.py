"""Target-query trace scenarios."""

from __future__ import annotations

import shutil

from .matrix import ROOT
from .common import (
    ATTACHED,
    OPEN_TRUE,
    command,
    declaration,
    event,
    invalid_request,
    lifecycle_prefix,
    prove_args,
    write_jsonl,
)


QUERY_KIND_INFO = {
    "Theorem": {
        "open": ("open_theorem", "True"),
        "done": ("done_theorem", "True"),
    },
    "Lemma": {
        "open": ("open_lemma", "True"),
        "done": ("done_lemma", "True"),
    },
    "Definition": {
        "open": ("open_definition", "nat"),
        "done": ("done_definition", "nat"),
    },
}

QUERY_OPEN = {
    "theorem": "Query.Main.open_theorem",
    "statement": "Theorem open_theorem : True",
    "status": "Open",
    "goals": OPEN_TRUE["goals"],
}

QUERY_DONE = {
    "theorem": "Query.Main.done_theorem",
    "statement": "Theorem done_theorem : True",
    "status": "Completed",
    "goals": "",
}

def query_selection_setup(selection: str) -> list[dict[str, object]]:
    if selection == "no_project":
        return []
    setup = [command("start", {"project_path": "query_project"}, ATTACHED)]
    if selection == "open":
        setup.append(command("prove", prove_args("Query.Main.open_theorem"), QUERY_OPEN))
    elif selection == "completed":
        setup.append(command("prove", prove_args("Query.Main.done_theorem"), QUERY_DONE))
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
    if target == "full_name":
        constant, _ = QUERY_KIND_INFO[declaration_kind]["done"]
        return f"Query.Main.{constant}"
    if target == "missing":
        return f"missing_{declaration_kind.lower()}"
    raise AssertionError(target)

def query_target_expected(kind: str, target: str, declaration_kind: str) -> object:
    requested = query_target_name(target, declaration_kind)
    if target == "missing":
        return {"kind": "not_found", "message": f"declaration '{requested}' was not found"}
    constant, ty = QUERY_KIND_INFO[declaration_kind]["done"]
    header = f"{declaration_kind} {constant} : {ty}"
    if kind == "statement":
        # PET's About output includes a disposable workspace path and source
        # location. Keep an explicit stable-field marker rather than reducing
        # the wrapper output to a locally formatted declaration header.
        return {
            "$pet_about": {
                "name": constant,
                "statement": ty,
                "constant": f"Main.{constant}",
            }
        }
    if kind in {"proof", "definition"}:
        return {
            "$pet_print": {
                "name": constant,
                "term": "42" if declaration_kind == "Definition" else "I",
                "type": ty,
            }
        }
    if kind == "assumptions":
        return {"text": "Closed under the global context"}
    transparency = "Transparent" if declaration_kind == "Definition" else "Opaque"
    return {
        "text": f"{transparency} constants:\n{constant} :\n{ty}"
    }

def materialize_query_target(records: list[dict[str, object]]) -> None:
    """Write source-backed target resolution through explicit declaration IDs."""
    directory = ROOT / "traces/basic/generated/query_target"
    if directory.exists():
        shutil.rmtree(directory)
    selected = records
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
                    {
                        "kind": kind,
                        "target": declaration(
                            query_target_name(target, declaration_kind)
                        ),
                    },
                    expected,
                )
            )
            record["implementation"] = "implemented"
        events.extend([event("user_disconnect", user="alice"), event("server_kill")])
        path = ROOT / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        write_jsonl(path, events)
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
        return invalid_request("pattern must be a non-empty string")
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
        write_jsonl(target, events)
