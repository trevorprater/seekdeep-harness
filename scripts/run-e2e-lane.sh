#!/usr/bin/env bash
# Runs the real-API end-to-end suites: every test target whose file reads DEEPSEEK_API_KEY.
#
# This is the port of vitest.e2e.config.ts. The credentialed suites are #[ignore] so the
# ordinary workspace gate never spends a credential by accident, and they self-skip without a
# key, so .github/workflows/e2e.yml owns proving one is present; this lane selects the targets
# that carry the keyed tests and runs them with ignored tests included. Like the source config,
# values may come from the environment or the gitignored root .env (the environment wins), each
# package gets two retries against transient API flakes, and SEEKDEEP_E2E_MAX_WORKERS (default 4)
# bounds the test threads. Selection is per test file rather than per crate, so a crate that
# also holds keyless suites does not run them here; cargo applies --test as a filter across
# every selected package, so the lane invokes cargo once per crate. Each package name comes
# from its own manifest because crate directories do not all share a prefix
# (crates/jsonrpc-demo is seekdeep-sdk-jsonrpc-demo). Leading arguments naming test files
# (crates/<crate>/tests/<name>.rs) select those targets instead, as the source passed a file to
# vitest for a provider-specific lane; `--` ends that selection, and every remaining argument
# goes to the test binaries.
set -euo pipefail
cd "$(dirname "$0")/.."

# Loads KEY=VALUE lines (comments, blank lines, `export` prefixes, and single or double quotes
# accepted) without overriding variables the environment already carries, as process.loadEnvFile does.
load_env_file() {
  local file="$1" line key value
  [ -f "$file" ] || return 0
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line#"${line%%[![:space:]]*}"}"
    case "$line" in ''|'#'*) continue ;; esac
    line="${line#export }"
    case "$line" in *=*) ;; *) continue ;; esac
    key="${line%%=*}"
    key="${key%"${key##*[![:space:]]}"}"
    case "$key" in ''|[0-9]*|*[!A-Za-z0-9_]*) continue ;; esac
    value="${line#*=}"
    value="${value#"${value%%[![:space:]]*}"}"
    case "$value" in
      \"*\") value="${value#\"}"; value="${value%\"}" ;;
      \'*\') value="${value#\'}"; value="${value%\'}" ;;
      *) value="${value%%[[:space:]]#*}"; value="${value%"${value##*[![:space:]]}"}" ;;
    esac
    if [ -z "${!key+set}" ]; then
      export "$key=$value"
    fi
  done < "$file"
}
load_env_file .env

max_workers="${SEEKDEEP_E2E_MAX_WORKERS:-}"
if [ -z "$max_workers" ]; then
  max_workers=4
elif ! [[ "$max_workers" =~ ^[1-9][0-9]*$ ]]; then
  echo "SEEKDEEP_E2E_MAX_WORKERS must be a positive integer, got \"$max_workers\"" >&2
  exit 1
fi

selected=()
while [ $# -gt 0 ]; do
  case "$1" in
    --) shift; break ;;
    crates/*/tests/*.rs)
      if [ ! -f "$1" ]; then
        echo "e2e lane: no such test file: $1" >&2
        exit 1
      fi
      selected+=("$1")
      shift
      ;;
    *) break ;;
  esac
done

# Only a crate's direct tests/*.rs files are cargo test targets; fixtures and support modules in
# subdirectories are not.
if [ "${#selected[@]}" -gt 0 ]; then
  files=$(printf '%s\n' "${selected[@]}" | sort -u)
else
  files=$(grep -l 'DEEPSEEK_API_KEY' crates/*/tests/*.rs | sort)
  if [ -z "$files" ]; then
    echo "e2e lane: no test target reads DEEPSEEK_API_KEY" >&2
    exit 1
  fi
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
  echo "e2e lane: $name (test threads $max_workers)" >&2
  code=0
  for attempt in 1 2 3; do
    if cargo test "${targets[@]}" -- --include-ignored --test-threads="$max_workers" "$@"; then
      code=0
      break
    else
      code=$?
    fi
    if [ "$attempt" -lt 3 ]; then
      echo "e2e lane: $name failed (attempt $attempt of 3), retrying" >&2
    fi
  done
  if [ "$code" -ne 0 ]; then
    status=$code
  fi
done
exit "$status"
