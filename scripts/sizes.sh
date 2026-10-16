#!/usr/bin/env bash
# Sizes to track across commits: the runtime (the release `cloison` binary), the
# stencil library it copies code from, and the code the JIT generates for
# each listing in bench/bytecode/ (summed over every compile, from
# `CLOISON_JIT_LOG`: hot code, out-of-line code (edge stubs, slow-path helpers,
# runtime-call wrappers), exits, and all of it).
#
#   scripts/sizes.sh            # print, with the change since the last record
#   scripts/sizes.sh --record   # also append this commit's values to bench/sizes.tsv
#
# The commit is HEAD's, with `+` if src/, stencils/ or build.rs differ from
# it; SIZES_COMMIT overrides it.
#
# bench/sizes.tsv has one `commit<TAB>metric<TAB>bytes` row per metric. Host
# (x86-64) only.
set -euo pipefail
cd "$(dirname "$0")/.."
record=false
[[ ${1:-} == --record ]] && record=true
cargo build --release --quiet --bin cloison
commit=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- src stencils build.rs Cargo.toml || commit="$commit+"
commit=${SIZES_COMMIT:-$commit}

section() { size -A "$1" | awk -v s="$2" '$1 == s { print $2 }'; }
obj=$(find target/release/build/cloison -name stencils.o -printf '%T@ %p\n' | sort -n | tail -1 | cut -d' ' -f2)

declare -a rows
rows+=("runtime.text $(section target/release/cloison .text)")
rows+=("runtime.rodata $(section target/release/cloison .rodata)")
rows+=("runtime.data.rel.ro $(section target/release/cloison .data.rel.ro)")
# Each stencil is in a section of its own (`.text.cloison_stencil_…`).
rows+=("stencils.text $(size -A "$obj" | awk '$1 ~ /^\.text/ { t += $2 } END { print t }')")
hot_all=0 ool_all=0 exits_all=0 total_all=0
for f in bench/bytecode/*.asm; do
  name=$(basename "$f" .asm)
  read -r hot ool exits total < <(CLOISON_JIT_LOG=1 target/release/cloison "$f" 2>&1 >/dev/null |
    sed -n 's/^jit .* (\([0-9]*\) hot, \([0-9]*\) out-of-line, \([0-9]*\) exits, \([0-9]*\) bytes).*/\1 \2 \3 \4/p' |
    awk '{ h += $1; o += $2; e += $3; t += $4 } END { print h + 0, o + 0, e + 0, t + 0 }')
  rows+=("jit.$name.hot $hot" "jit.$name.ool $ool" "jit.$name.exits $exits" "jit.$name.total $total")
  hot_all=$((hot_all + hot)) ool_all=$((ool_all + ool)) exits_all=$((exits_all + exits)) total_all=$((total_all + total))
done
rows+=("jit.all.hot $hot_all" "jit.all.ool $ool_all" "jit.all.exits $exits_all" "jit.all.total $total_all")

tsv=bench/sizes.tsv
last=""
[[ -f $tsv ]] && last=$(awk -F'\t' 'NR > 1 { c = $1 } END { print c }' "$tsv")
printf '%-40s %10s %10s %8s\n' "metric ($commit)" bytes "${last:-—}" change
for r in "${rows[@]}"; do
  read -r metric bytes <<<"$r"
  before=""
  [[ -n $last ]] && before=$(awk -F'\t' -v c="$last" -v m="$metric" '$1 == c && $2 == m { print $3 }' "$tsv")
  change=""
  [[ -n $before && $before != 0 ]] && change=$(awk -v a="$before" -v b="$bytes" 'BEGIN { printf "%+.1f%%", 100 * (b - a) / a }')
  printf '%-40s %10s %10s %8s\n' "$metric" "$bytes" "${before:-}" "$change"
done
if $record; then
  [[ -f $tsv ]] || printf 'commit\tmetric\tbytes\n' >"$tsv"
  for r in "${rows[@]}"; do
    read -r metric bytes <<<"$r"
    printf '%s\t%s\t%s\n' "$commit" "$metric" "$bytes" >>"$tsv"
  done
  echo "recorded $commit in $tsv"
fi
