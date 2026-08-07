#!/usr/bin/env bash
# Keep the ordinary CI and manual desktop-build workflows on one dependency
# graph. Coordinated release workflows intentionally use release tags and are
# excluded from this check; their pins change only during a release cut.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
canonical="$repo_root/.github/workflows/ci.yml"
manual_build="$repo_root/.github/workflows/build-desktop.yml"

if [[ ! -f "$canonical" || ! -f "$manual_build" ]]; then
  echo "error: expected ci.yml and build-desktop.yml below $repo_root/.github/workflows" >&2
  exit 1
fi

pin_keys=(
  RATSPEAK_RSRETICULUM_REF
  RATSPEAK_RSLXMF_REF
  RATSPEAK_RSLXST_REF
  RATSPEAK_LRGP_REF
)

pin_from_workflow() {
  local workflow="$1"
  local key="$2"
  awk -v key="$key" '$1 == key ":" { print $2; exit }' "$workflow"
}

status=0
for key in "${pin_keys[@]}"; do
  ci_pin="$(pin_from_workflow "$canonical" "$key")"
  build_pin="$(pin_from_workflow "$manual_build" "$key")"

  if [[ ! "$ci_pin" =~ ^[0-9a-f]{40}$ ]]; then
    echo "error: $key in ci.yml must be a 40-character commit SHA (found: ${ci_pin:-missing})" >&2
    status=1
  fi
  if [[ ! "$build_pin" =~ ^[0-9a-f]{40}$ ]]; then
    echo "error: $key in build-desktop.yml must be a 40-character commit SHA (found: ${build_pin:-missing})" >&2
    status=1
  fi
  if [[ "$ci_pin" != "$build_pin" ]]; then
    echo "error: $key differs: ci.yml=$ci_pin build-desktop.yml=$build_pin" >&2
    status=1
  fi
done

if [[ "$status" -ne 0 ]]; then
  exit "$status"
fi

echo "ordinary workflow dependency pins: aligned"
