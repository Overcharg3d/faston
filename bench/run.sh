#!/usr/bin/env bash
# Usage: bench/run.sh  — times each benchmark interpreted, compiled to native
# code, and in CPython (best of 3 runs each)
cd "$(dirname "$0")"
best() {
  local b=999
  for k in 1 2 3; do
    local s=$(date +%s.%N); "$@" > /tmp/faston_bench_out; local e=$(date +%s.%N)
    b=$(echo "x = $e - $s; if (x < $b) x else $b" | bc)
  done
  printf "%6.2fs" "$b"
}
printf "%-10s %10s %10s %10s   %s\n" benchmark interpreted native CPython output
for b in fib loop objects lists particles strings; do
  i=$(best ../target/release/faston $b.fst)
  n=$(best ../target/release/faston run --native $b.fst)
  out=$(tr '\n' ' ' < /tmp/faston_bench_out)
  p=$(best python3 $b.py)
  printf "%-10s %10s %10s %10s   %s\n" $b "$i" "$n" "$p" "$out"
done
