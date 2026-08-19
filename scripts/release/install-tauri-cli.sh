#!/usr/bin/env bash
# Reuse the exact reviewed Tauri CLI from the trusted default-branch Cargo
# cache, compiling it only on a cache miss or reviewed version change.
set -euo pipefail

expected_version="${1:?usage: install-tauri-cli.sh VERSION}"
expected_output="tauri-cli $expected_version"

if command -v cargo-tauri >/dev/null 2>&1 &&
  [[ "$(cargo tauri --version)" == "$expected_output" ]]; then
  echo "Using cached $expected_output"
else
  cargo install tauri-cli --version "$expected_version" --locked
fi

actual_output="$(cargo tauri --version)"
if [[ "$actual_output" != "$expected_output" ]]; then
  echo "Expected $expected_output, got $actual_output" >&2
  exit 1
fi
