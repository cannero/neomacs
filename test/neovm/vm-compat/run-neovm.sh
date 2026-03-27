#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <forms-file>" >&2
  exit 2
fi

forms_file="$1"
if [[ ! -f "$forms_file" ]]; then
  echo "forms file not found: $forms_file" >&2
  exit 2
fi

forms_dir="$(cd "$(dirname "$forms_file")" && pwd)"
forms_file_abs="$forms_dir/$(basename "$forms_file")"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
worker_manifest="$repo_root/neovm-worker/Cargo.toml"
worker_binary="$repo_root/neovm-worker/target/debug/examples/elisp_compat_runner"
worker_feature_stamp="$repo_root/neovm-worker/target/debug/examples/elisp_compat_runner.features"
worker_features="${NEOVM_WORKER_CARGO_FEATURES:-}"

needs_build=0
if [[ ! -x "$worker_binary" ]]; then
  needs_build=1
elif find \
  "$repo_root/neovm-core" \
  "$repo_root/neovm-host-abi" \
  "$repo_root/neovm-worker" \
  -type f \
  \( -name '*.rs' -o -name 'Cargo.toml' \) \
  -newer "$worker_binary" \
  -print -quit | grep -q .; then
  needs_build=1
elif [[ ! -f "$worker_feature_stamp" ]]; then
  needs_build=1
elif [[ "$(<"$worker_feature_stamp")" != "$worker_features" ]]; then
  needs_build=1
fi

if [[ "$needs_build" -eq 1 ]]; then
  build_cmd=(
    cargo build
    --manifest-path "$worker_manifest"
    --example elisp_compat_runner
  )
  if [[ -n "$worker_features" ]]; then
    build_cmd+=(--features "$worker_features")
  fi
  "${build_cmd[@]}"
  printf '%s' "$worker_features" > "$worker_feature_stamp"
fi

NEOVM_FORMS_FILE="$forms_file_abs" \
"$worker_binary" "$forms_file_abs" \
  | LC_ALL=C awk -f "$script_dir/filter-case-lines.awk"
