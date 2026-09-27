#!/bin/sh
set -eu

# Build and install the pinned PET source without depending on Dune's private
# _build layout.  The generated launcher selects the matching coq-lsp findlib
# tree, so PET cannot accidentally dynlink plugins from another installation.
repository=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd -P)
source_tree="$repository/third_party/coq-lsp"
prefix=${ROCQ_PET_PREFIX:-"$repository/target/pet"}
jobs=${ROCQ_PET_BUILD_JOBS:-4}

case "$prefix" in
  /*) ;;
  *) prefix="$repository/$prefix" ;;
esac

if [ ! -f "$source_tree/dune-project" ]; then
  printf '%s\n' 'third_party/coq-lsp is absent; run git submodule update --init' >&2
  exit 1
fi

mkdir -p "$prefix/bin"
(
  cd "$source_tree"
  opam exec -- dune build -j "$jobs" -p coq-lsp
  opam exec -- dune install --prefix "$prefix" coq-lsp
)

launcher="$prefix/bin/rocq-mcp-pet"
cat >"$launcher" <<EOF
#!/bin/sh
OCAMLPATH='$prefix/lib' \
  exec '$prefix/bin/pet' "\$@"
EOF
chmod 755 "$launcher"

printf 'PET launcher: %s\n' "$launcher"
printf 'Use it with: export ROCQ_PET_BIN=%s\n' "$launcher"
