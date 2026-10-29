#!/usr/bin/env bash
# Runs the whole test suite, then checks that every test named in ATTACKS.md
# ran and passed. Fails if any test fails or any named test is missing.
set -uo pipefail
cd "$(dirname "$0")/.."

log=$(mktemp)
trap 'rm -f "$log"' EXIT

cargo test --workspace --locked --no-fail-fast > "$log" 2>&1
status=$?

names=$(grep -oE '`[a-z0-9_]+`' ATTACKS.md | tr -d '`' | sort -u)
total=$(printf '%s\n' "$names" | wc -l)
missing=0
for name in $names; do
  if ! grep -qE "^test (.+::)?${name} \.\.\. ok$" "$log"; then
    echo "NOT PASSING: $name"
    missing=$((missing + 1))
  fi
done

if [ "$status" -ne 0 ]; then
  grep -E "^test .* FAILED$|panicked at|^---- " "$log" | head -40
  echo "attack suite: the test suite failed"
  exit 1
fi
if [ "$missing" -ne 0 ]; then
  echo "attack suite: $missing of $total named tests did not pass"
  exit 1
fi
echo "attack suite: all $total named tests passed"
