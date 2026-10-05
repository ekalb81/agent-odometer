#!/usr/bin/env bash
# The experiment copies the current production command lists, never replacing them.
set -euo pipefail
lane="$1"
record="$2"
mkdir -p "$record"
step() {
  local label="$1"; shift
  local start finish status=0
  start=$(date +%s%3N)
  "$@" || status=$?
  finish=$(date +%s%3N)
  printf '%s\t%s\t%s\n' "$label" "$((finish-start))" "$status" >> "$record/commands.tsv"
  return "$status"
}
case "$lane" in
  check)
    step npm-ci npm ci
    step frontend-check npm run check
    step frontend-coverage npm run test:coverage
    step updater-manifest npm run check:updater-manifest
    step frontend-build npm run build
    cd src-tauri
    step rust-format cargo fmt --check
    step rust-clippy cargo clippy --all-targets --locked -- -D warnings
    step rust-tests cargo test --locked
    ;;
  msrv)
    cd src-tauri
    step msrv-check cargo check --locked
    ;;
  coverage)
    cd src-tauri
    step rust-coverage cargo llvm-cov --locked --lcov --output-path lcov.info
    step coverage-summary cargo llvm-cov report --summary-only
    ;;
  *) echo 'Unsupported lane' >&2; exit 2 ;;
esac
