#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/build-runtime-images.sh [--bin-dir DIR] [--runtime-root DIR] [--dry-run]

Build the GNU-shaped Neomacs runtime pipeline:
  1. neomacs-temacs --temacs=pbootstrap
  2. bootstrap-neomacs generates loaddefs / ldefs-boot
  3. bootstrap-neomacs warms the GNU compile-first set into .neobc cache files
  4. neomacs-temacs --temacs=pdump

Options:
  --bin-dir DIR       Directory containing neomacs-temacs/bootstrap-neomacs/neomacs
  --runtime-root DIR  Runtime root containing lisp/ and etc/
  --dry-run           Print planned commands without running them

Environment:
  NEOMACS_NATIVE_COMP=yes
      Include the native-comp-only COMPILE_FIRST entries from lisp/Makefile.in.
EOF
}

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)
runtime_root="$repo_root"
bin_dir="$repo_root/target/debug"
dry_run=0
native_comp=${NEOMACS_NATIVE_COMP:-no}

while (($#)); do
  case "$1" in
    --bin-dir)
      bin_dir="$2"
      shift 2
      ;;
    --runtime-root)
      runtime_root="$2"
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

temacs="$bin_dir/neomacs-temacs"
bootstrap="$bin_dir/bootstrap-neomacs"
final_bin="$bin_dir/neomacs"
lisp_root="$runtime_root/lisp"
makefile_in="$lisp_root/Makefile.in"

for required in "$temacs" "$bootstrap" "$final_bin" "$lisp_root/loadup.el" "$makefile_in"; do
  if [[ ! -e "$required" ]]; then
    echo "missing required path: $required" >&2
    exit 1
  fi
done

run_cmd() {
  printf '+'
  for arg in "$@"; do
    printf ' %q' "$arg"
  done
  printf '\n'
  if ((dry_run == 0)); then
    "$@"
  fi
}

mapfile -t loaddefs_dirs < <(
  find "$lisp_root" \
    \( -path "$lisp_root/obsolete" -o -path "$lisp_root/obsolete/*" -o \
       -path "$lisp_root/term" -o -path "$lisp_root/term/*" \) -prune -o \
    -type d -print | LC_ALL=C sort
)

mapfile -t compile_first_sources < <(
  awk -v lisp_root="$lisp_root" -v native_comp="$native_comp" '
    function emit(line,    n, i, token, path) {
      gsub(/\\/, "", line)
      n = split(line, parts, /[[:space:]]+/)
      for (i = 1; i <= n; i++) {
        token = parts[i]
        if (token == "" || token !~ /^\$\(lisp\)\//) {
          continue
        }
        sub(/^\$\(lisp\)\//, lisp_root "/", token)
        sub(/\.elc$/, ".el", token)
        print token
      }
    }
    /^ifeq \(\$\(HAVE_NATIVE_COMP\),yes\)/ {
      in_native_block = 1
      next
    }
    /^endif$/ {
      in_native_block = 0
      next
    }
    /^COMPILE_FIRST[[:space:]]*\+?=/ {
      if (in_native_block && native_comp != "yes") {
        capture = 0
        next
      }
      capture = 1
      line = $0
      sub(/^COMPILE_FIRST[[:space:]]*\+?=[[:space:]]*/, "", line)
      emit(line)
      if ($0 !~ /\\$/) {
        capture = 0
      }
      next
    }
    capture {
      emit($0)
      if ($0 !~ /\\$/) {
        capture = 0
      }
    }
  ' "$makefile_in" | awk '!seen[$0]++'
)

existing_compile_first_sources=()
for source in "${compile_first_sources[@]}"; do
  if [[ -f "$source" ]]; then
    existing_compile_first_sources+=("$source")
  fi
done

export NEOMACS_RUNTIME_ROOT="$runtime_root"

run_cmd "$temacs" --batch -l loadup --temacs=pbootstrap

run_cmd "$bootstrap" --batch \
  -l "$lisp_root/emacs-lisp/loaddefs-gen.el" \
  -f loaddefs-generate--emacs-batch \
  "${loaddefs_dirs[@]}"

printf '+ %q\n' "sed '/^;; Local Variables:/a\\
;; no-byte-compile: t' < '$lisp_root/loaddefs.el' > '$lisp_root/ldefs-boot.el'"
if ((dry_run == 0)); then
  sed '/^;; Local Variables:/a\
;; no-byte-compile: t' \
    < "$lisp_root/loaddefs.el" \
    > "$lisp_root/ldefs-boot.el"
fi

if ((${#existing_compile_first_sources[@]} > 0)); then
  compile_first_args=(--batch)
  for source in "${existing_compile_first_sources[@]}"; do
    compile_first_args+=(-l "$source")
  done
  run_cmd "$bootstrap" "${compile_first_args[@]}"
fi

run_cmd "$temacs" --batch -l loadup --temacs=pdump
