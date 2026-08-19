#!/usr/bin/env bash
# Prove that ordinary CI pins and release checkouts use the reviewed source
# graph in release/dependency-set.json.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
canonical="$repo_root/.github/workflows/ci.yml"
manual_build="$repo_root/.github/workflows/build-desktop.yml"
release_workflows=(
  "$repo_root/.github/workflows/release-android.yml"
  "$repo_root/.github/workflows/release-desktop.yml"
  "$repo_root/.github/workflows/release-ios.yml"
  "$repo_root/.github/workflows/release-macos.yml"
  "$repo_root/.github/workflows/release-windows.yml"
)

if [[ ! -f "$canonical" || ! -f "$manual_build" ]]; then
  echo "error: expected ci.yml and build-desktop.yml below $repo_root/.github/workflows" >&2
  exit 1
fi

reviewed_outputs="$(node "$repo_root/scripts/release/source-integrity.mjs" github-outputs)"
pin_keys=(
  RATSPEAK_RSRETICULUM_REF
  RATSPEAK_RSLXMF_REF
  RATSPEAK_RSLXST_REF
  RATSPEAK_LRGP_REF
)
output_keys=(rsreticulum_ref rslxmf_ref rslxst_ref lrgp_ref)

pin_from_workflow() {
  local workflow="$1"
  local key="$2"
  awk -v key="$key" '$1 == key ":" { print $2; exit }' "$workflow"
}

reviewed_pin() {
  local key="$1"
  awk -F= -v key="$key" '$1 == key { print $2; exit }' <<< "$reviewed_outputs"
}

status=0
expected_node="$(reviewed_pin node_version)"
if [[ ! "$expected_node" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: dependency set has invalid node_version (found: ${expected_node:-missing})" >&2
  status=1
fi
if ! grep -q "node-version: $expected_node" "$canonical"; then
  echo "error: ci.yml must use reviewed Node $expected_node" >&2
  status=1
fi

for index in "${!pin_keys[@]}"; do
  key="${pin_keys[$index]}"
  expected="$(reviewed_pin "${output_keys[$index]}")"
  ci_pin="$(pin_from_workflow "$canonical" "$key")"
  build_pin="$(pin_from_workflow "$manual_build" "$key")"

  if [[ ! "$expected" =~ ^[0-9a-f]{40}$ ]]; then
    echo "error: dependency set has invalid ${output_keys[$index]} (found: ${expected:-missing})" >&2
    status=1
  fi
  if [[ "$ci_pin" != "$expected" ]]; then
    echo "error: $key in ci.yml must equal reviewed commit $expected (found: ${ci_pin:-missing})" >&2
    status=1
  fi
  if [[ "$build_pin" != "$expected" ]]; then
    echo "error: $key in build-desktop.yml must equal reviewed commit $expected (found: ${build_pin:-missing})" >&2
    status=1
  fi
done

for workflow in "${release_workflows[@]}"; do
  if ! grep -q "node-version: $expected_node" "$workflow"; then
    echo "error: $(basename "$workflow") must use reviewed Node $expected_node" >&2
    status=1
  fi
  if grep -q 'RATSPEAK_.*_REF:' "$workflow"; then
    echo "error: $(basename "$workflow") declares a second component-pin source" >&2
    status=1
  fi
  if ! grep -q 'source-integrity.mjs github-outputs' "$workflow"; then
    echo "error: $(basename "$workflow") does not load the reviewed dependency set" >&2
    status=1
  fi
  if ! grep -q 'source-integrity.mjs verify-release-source' "$workflow"; then
    echo "error: $(basename "$workflow") does not verify checked-out component commits" >&2
    status=1
  fi
  if [[ "$(basename "$workflow")" != "release-ios.yml" ]]; then
    if ! grep -q 'source-integrity.mjs verify-release-ref' "$workflow"; then
      echo "error: $(basename "$workflow") does not verify the exact annotated publishing ref" >&2
      status=1
    fi
    if ! grep -q 'QUALIFY_RELEASE_REF' "$workflow"; then
      echo "error: $(basename "$workflow") does not bind qualified BOMs to the verified ref" >&2
      status=1
    fi
  fi
  for output_key in "${output_keys[@]}"; do
    if ! grep -q "steps.source.outputs.$output_key" "$workflow"; then
      echo "error: $(basename "$workflow") does not consume $output_key" >&2
      status=1
    fi
  done
done

if [[ "$status" -ne 0 ]]; then
  exit "$status"
fi

echo "workflow dependency pins: reviewed source graph enforced"
