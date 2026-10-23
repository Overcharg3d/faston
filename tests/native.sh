#!/usr/bin/env bash
# Runs every test program compiled to native code (FASTON_NATIVE=1), comparing
# against the same expected output as the interpreter. Extra env (e.g.
# FASTON_GC_STRESS=1) is passed through.
cd "$(dirname "$0")/.."
bin=./target/release/faston
fail=0
for f in tests/cases/[a-z]*.fst; do
  out=$(FASTON_NATIVE=1 timeout 60 $bin "$f" 2>&1)
  if [ "$out" != "$(cat "${f%.fst}.out")" ]; then
    echo "FAIL $f"; diff <(echo "$out") "${f%.fst}.out" | head -10; fail=1
  fi
done
for f in tests/errors/[a-z]*.fst; do
  err=$(FASTON_NATIVE=1 timeout 60 $bin "$f" 2>&1 >/dev/null)
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    if ! grep -qF -- "$line" <<<"$err"; then echo "FAIL $f: missing '$line'"; echo "$err" | head -6; fail=1; fi
  done < "${f%.fst}.err"
done
[ $fail = 0 ] && echo "all native tests passed"
exit $fail
