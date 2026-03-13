#!/usr/bin/env bash
# Run bold overlap test with debug logging
# Usage: ./test/neomacs/run-bold-overlap-test.sh
set -e
cd "$(git rev-parse --show-toplevel)"
RUST_LOG=debug exec ./src/emacs -Q -l test/neomacs/bold-overlap-test.el
