#!/usr/bin/env python3
"""Generate proof-free Rocq .v spec views with `rocq doc --raw --light`."""

from __future__ import annotations

import argparse
import os
import shlex
import shutil
import subprocess
import sys
from pathlib import Path


DEFAULT_EXCLUDE_DIRS = {
    ".git",
    ".hg",
    ".svn",
    "_build",
    "_opam",
    ".direnv",
    ".venv",
    "node_modules",
}


def parse_args() -> tuple[argparse.Namespace, list[str]]:
    parser = argparse.ArgumentParser(
        description=(
            "Extract all .v files in a Rocq/Coq codebase into a proof-free "
            "spec view using `rocq doc --raw --light --stdout`."
        )
    )
    parser.add_argument("repo", help="Rocq/Coq repository or source directory")
    parser.add_argument("output_dir", help="Directory to receive generated .v files")
    parser.add_argument(
        "--rocq-bin",
        default="rocq",
        help="Rocq command to run (default: rocq)",
    )
    parser.add_argument(
        "--project-file",
        help="Optional _RocqProject/_CoqProject file to read -Q/-R/-I flags from",
    )
    parser.add_argument(
        "--no-auto-project-file",
        action="store_true",
        help="Do not auto-read <repo>/_RocqProject or <repo>/_CoqProject",
    )
    parser.add_argument(
        "--clean-output",
        action="store_true",
        help="Remove the output directory first",
    )
    parser.add_argument(
        "--exclude-dir",
        action="append",
        default=[],
        help="Directory basename to exclude while walking sources; repeatable",
    )
    parser.add_argument(
        "--include-hidden",
        action="store_true",
        help="Include hidden directories other than explicitly excluded dirs",
    )
    parser.add_argument(
        "--files-from",
        help="Optional newline-delimited file list, relative to repo unless absolute",
    )

    args, extra = parser.parse_known_args()
    if extra and extra[0] == "--":
        extra = extra[1:]
    return args, extra


def resolve_repo(path: str) -> Path:
    repo = Path(path).expanduser().resolve()
    if not repo.exists():
        raise SystemExit(f"repo does not exist: {repo}")
    if not repo.is_dir():
        raise SystemExit(f"repo is not a directory: {repo}")
    return repo


def prepare_output(out_dir: Path, clean: bool) -> None:
    if clean and out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)


def strip_project_comment(line: str) -> str:
    # _CoqProject/_RocqProject comments are commonly shell-style '#'.
    return line.split("#", 1)[0].strip()


def resolve_project_path(base: Path, value: str) -> str:
    path = Path(value).expanduser()
    if path.is_absolute():
        return str(path)
    return str((base / path).resolve())


def read_project_args(project_file: Path) -> list[str]:
    base = project_file.parent.resolve()
    tokens: list[str] = []
    for raw_line in project_file.read_text(encoding="utf-8").splitlines():
        line = strip_project_comment(raw_line)
        if not line:
            continue
        tokens.extend(shlex.split(line))

    args: list[str] = []
    i = 0
    while i < len(tokens):
        tok = tokens[i]
        if tok in {"-Q", "-R"} and i + 2 < len(tokens):
            args.extend([tok, resolve_project_path(base, tokens[i + 1]), tokens[i + 2]])
            i += 3
        elif tok == "-I" and i + 1 < len(tokens):
            args.extend([tok, resolve_project_path(base, tokens[i + 1])])
            i += 2
        else:
            i += 1
    return args


def auto_project_file(repo: Path) -> Path | None:
    for name in ("_RocqProject", "_CoqProject"):
        candidate = repo / name
        if candidate.is_file():
            return candidate
    return None


def read_file_list(repo: Path, file_list: Path) -> list[Path]:
    base = file_list.parent.resolve()
    result: list[Path] = []
    for raw_line in file_list.read_text(encoding="utf-8").splitlines():
        line = raw_line.split("#", 1)[0].strip()
        if not line:
            continue
        path = Path(line).expanduser()
        if not path.is_absolute():
            path = (repo / path).resolve()
            if not path.exists():
                path = (base / line).resolve()
        if path.suffix == ".v" and path.is_file():
            result.append(path)
    return sorted(dict.fromkeys(result))


def discover_v_files(
    repo: Path,
    out_dir: Path,
    excludes: set[str],
    include_hidden: bool,
) -> list[Path]:
    out_inside_repo = False
    try:
        out_dir.relative_to(repo)
        out_inside_repo = True
    except ValueError:
        pass

    files: list[Path] = []
    for root, dirnames, filenames in os.walk(repo):
        root_path = Path(root)
        filtered = []
        for dirname in dirnames:
            if dirname in excludes:
                continue
            if not include_hidden and dirname.startswith("."):
                continue
            child = root_path / dirname
            if out_inside_repo:
                try:
                    child.resolve().relative_to(out_dir)
                    continue
                except ValueError:
                    pass
            filtered.append(dirname)
        dirnames[:] = filtered

        for filename in filenames:
            if filename.endswith(".v"):
                files.append(root_path / filename)
    return sorted(files)


def run_rocq_doc(
    rocq_bin: str,
    rocq_args: list[str],
    src: Path,
    cwd: Path,
) -> subprocess.CompletedProcess[str]:
    cmd = [
        rocq_bin,
        "doc",
        "--raw",
        "--light",
        "--stdout",
        *rocq_args,
        str(src),
    ]
    return subprocess.run(
        cmd,
        cwd=str(cwd),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def write_text(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def main() -> int:
    args, extra_rocq_args = parse_args()
    repo = resolve_repo(args.repo)
    out_dir = Path(args.output_dir).expanduser().resolve()

    prepare_output(out_dir, args.clean_output)

    project_args: list[str] = []
    project_file: Path | None = None
    if args.project_file:
        project_file = Path(args.project_file).expanduser().resolve()
    elif not args.no_auto_project_file:
        project_file = auto_project_file(repo)
    if project_file:
        if not project_file.is_file():
            raise SystemExit(f"project file does not exist: {project_file}")
        project_args = read_project_args(project_file)

    if args.files_from:
        files = read_file_list(repo, Path(args.files_from).expanduser().resolve())
    else:
        excludes = DEFAULT_EXCLUDE_DIRS | set(args.exclude_dir)
        files = discover_v_files(repo, out_dir, excludes, args.include_hidden)

    rocq_args = [*project_args, *extra_rocq_args]
    generated = 0
    failures = 0

    for src in files:
        try:
            rel = src.resolve().relative_to(repo)
        except ValueError:
            rel = Path(src.name)
        dst = out_dir / rel

        proc = run_rocq_doc(args.rocq_bin, rocq_args, src.resolve(), repo)
        if proc.stderr.strip():
            print(f"===== {rel} =====", file=sys.stderr)
            print(proc.stderr.rstrip(), file=sys.stderr)

        if proc.returncode == 0:
            header = (
                "(* Generated proof-free spec view.\n"
                f"   Source: {rel}\n"
                "   Command: rocq doc --raw --light --stdout\n"
                "*)\n\n"
            )
            write_text(dst, header + proc.stdout)
            generated += 1
        else:
            failures += 1
            print(f"ERROR: {rel} failed with exit code {proc.returncode}", file=sys.stderr)
            if proc.stdout.strip():
                print(proc.stdout.rstrip(), file=sys.stderr)

    if failures:
        print(
            f"Generated {generated}/{len(files)} files; {failures} failed.",
            file=sys.stderr,
        )
        return 1

    print(f"Generated {generated} proof-free .v files in {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
