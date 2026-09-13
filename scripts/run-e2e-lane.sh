#!/usr/bin/env bash
# Runs the real-API end-to-end suites: every test target whose file reads DEEPSEEK_API_KEY.
#
# The suites self-skip without a key, so .github/workflows/e2e.yml owns proving one is
# present; this lane only selects the targets that carry the keyed tests. Selection is per
# test file rather than per crate, so a crate that also holds keyless suites does not run
# them here; cargo applies --test as a filter across every selected package, so the lane
# invokes cargo once per crate. Each package name comes from its own manifest because
# crate directories do not all share a prefix (crates/jsonrpc-demo is
# seekdeep-sdk-jsonrpc-demo).
set -euo pipefail
cd "$(dirname "$0")/.."

files=$(grep -rl 'DEEPSEEK_API_KEY' --include='*.rs' crates/*/tests/ | sort)
if [ -z "$files" ]; then
  echo "e2e lane: no test target reads DEEPSEEK_API_KEY" >&2
  exit 1
fi

status=0
for directory in $(printf '%s\n' "$files" | cut -d/ -f2 | sort -u); do
  name=$(sed -n 's/^name = "\(.*\)"/\1/p' "crates/$directory/Cargo.toml" | head -1)
  if [ -z "$name" ]; then
    echo "e2e lane: crates/$directory/Cargo.toml declares no package name" >&2
    exit 1
  fi
  targets=(--package "$name")
  for stem in $(printf '%s\n' "$files" | grep "^crates/$directory/" | sed 's|.*/||; s|\.rs$||'); do
    targets+=(--test "$stem")
  done
  echo "e2e lane: $name" >&2
  cargo test "${targets[@]}" "$@" || status=$?
done
exit "$status"
