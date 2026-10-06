#!/usr/bin/env bash
# Measure the dispatch_bench matrix (program × codegen × engine) with
# hardware counters, and print a markdown table of instructions and cycles
# per loop iteration. Engines:
#   interp      interpreter without accumulator type state (--no-default-features)
#   interp+acc  interpreter with acc-i32 (default build)
#   jit1        copy-and-patch JIT, generic stencils only (milestone 1)
#   jit2        + type-specialized stencils (milestone 2)
#   jit         + VM registers in machine registers (milestone 3)
#
# Per-iteration numbers are (run at N) − (run at 1), divided by N − 1, which
# cancels process startup. Each cell is the minimum over REPS runs, pinned
# to one CPU.
#
#   scripts/bench.sh            # N=100000000 REPS=3 CPU=2
#   ENGINES='jit2 jit' PROGRAMS='int double' scripts/bench.sh
set -euo pipefail
cd "$(dirname "$0")/.."
N=${N:-100000000}
REPS=${REPS:-3}
CPU=${CPU:-2}

cargo build --release --quiet --bin dispatch_bench
CARGO_TARGET_DIR=target/no-acc-i32 cargo build --release --quiet --bin dispatch_bench --no-default-features
declare -A RUN=(
  [interp]="target/no-acc-i32/release/dispatch_bench %s %s %s interp"
  [interp+acc]="target/release/dispatch_bench %s %s %s interp"
  [jit1]="target/release/dispatch_bench %s %s %s jit1"
  [jit2]="target/release/dispatch_bench %s %s %s jit2"
  [jit]="target/release/dispatch_bench %s %s %s jit"
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

echo "| program | codegen | engine | instr/iter | cycles/iter | IPC |"
echo "|---|---|---|---:|---:|---:|"
for program in ${PROGRAMS:-int const chain double wide}; do
  for codegen in generic smi; do
    for engine in ${ENGINES:-interp interp+acc jit1 jit2 jit}; do
      # shellcheck disable=SC2059,SC2046
      read -r i1 c1 < <(counters $(printf "${RUN[$engine]}" "$program" "$codegen" 1))
      # shellcheck disable=SC2059,SC2046
      read -r iN cN < <(counters $(printf "${RUN[$engine]}" "$program" "$codegen" "$N"))
      awk -v p="$program" -v g="$codegen" -v a="$engine" -v n="$N" \
          -v i1="$i1" -v c1="$c1" -v iN="$iN" -v cN="$cN" 'BEGIN {
        ipi = (iN - i1) / (n - 1); cpi = (cN - c1) / (n - 1)
        printf "| %s | %s | %s | %.2f | %.2f | %.2f |\n", p, g, a, ipi, cpi, ipi / cpi
      }'
    done
  done
done
