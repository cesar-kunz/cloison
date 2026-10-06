#!/usr/bin/env bash
# Measure the dispatch_bench matrix (program × codegen × acc-i32 on/off)
# with hardware counters, and print a markdown table of instructions and
# cycles per loop iteration.
#
# Per-iteration numbers are (run at N) − (run at 1), divided by N − 1, which
# cancels process startup. Each cell is the minimum over REPS runs, pinned
# to one CPU.
#
#   scripts/bench.sh            # N=100000000 REPS=3 CPU=2
set -euo pipefail
cd "$(dirname "$0")/.."
N=${N:-100000000}
REPS=${REPS:-3}
CPU=${CPU:-2}

cargo build --release --quiet --bin dispatch_bench
CARGO_TARGET_DIR=target/no-acc-i32 cargo build --release --quiet --bin dispatch_bench --no-default-features
declare -A BIN=(
  [on]=target/release/dispatch_bench
  [off]=target/no-acc-i32/release/dispatch_bench
)

# Print "instructions cycles" for one run: the minimum of each over REPS.
counters() {
  local best_i= best_c=
  for _ in $(seq "$REPS"); do
    read -r i c < <(taskset -c "$CPU" perf stat -x, -e instructions:u,cycles:u "$@" 2>&1 >/dev/null |
      awk -F, '/instructions/ {i=$1} /cycles/ {c=$1} END {print i, c}')
    [[ -z $best_i || $i -lt $best_i ]] && best_i=$i
    [[ -z $best_c || $c -lt $best_c ]] && best_c=$c
  done
  echo "$best_i $best_c"
}

echo "| program | codegen | acc-i32 | instr/iter | cycles/iter | IPC |"
echo "|---|---|---|---:|---:|---:|"
for program in int const chain double; do
  for codegen in generic smi; do
    for acc in off on; do
      read -r i1 c1 < <(counters "${BIN[$acc]}" "$program" "$codegen" 1)
      read -r iN cN < <(counters "${BIN[$acc]}" "$program" "$codegen" "$N")
      awk -v p="$program" -v g="$codegen" -v a="$acc" -v n="$N" \
          -v i1="$i1" -v c1="$c1" -v iN="$iN" -v cN="$cN" 'BEGIN {
        ipi = (iN - i1) / (n - 1); cpi = (cN - c1) / (n - 1)
        printf "| %s | %s | %s | %.2f | %.2f | %.2f |\n", p, g, a, ipi, cpi, ipi / cpi
      }'
    done
  done
done
