#!/usr/bin/env bash
# Time and peak RSS of Octane's deterministic runs (as scripts/octane.sh
# assembles them into target/octane/<b>-run.js; OCTANE_DIR overrides), one
# line per benchmark: seconds, MB, the run's value.
#
#   scripts/gc-bench.sh                  # every benchmark that runs
#   scripts/gc-bench.sh splay deltablue  # only these
#   CLOISON=path/to/cloison scripts/gc-bench.sh  # another build
set -uo pipefail
cd "$(dirname "$0")/.."
dir=${OCTANE_DIR:-target/octane}
cloison=${CLOISON:-target/release/cloison}
order="richards deltablue crypto raytrace earley-boyer regexp splay
  navier-stokes pdfjs gbemu code-load box2d typescript"
[ $# -gt 0 ] && order="$*"
printf '%-14s %8s %8s  %s\n' benchmark seconds MB value
for b in $order; do
  f=$dir/$b-run.js
  [ -f "$f" ] || { printf '%-14s (no %s)\n' "$b" "$f"; continue; }
  out=$(mktemp)
  if [ "$(uname)" = Darwin ]; then
    /usr/bin/time -l "$cloison" "$f" > "$out.stdout" 2> "$out"
    kb=$(awk '/maximum resident set size/ {print int($1 / 1024)}' "$out")
    s=$(awk '/ real / {print $1}' "$out")
  else
    /usr/bin/time -f '%e %M' "$cloison" "$f" > "$out.stdout" 2> "$out"
    read -r s kb < <(tail -n 1 "$out")
  fi
  v=$(tail -n 1 "$out.stdout" | cut -c1-60)
  printf '%-14s %8s %8d  %s\n' "$b" "$s" "$((kb / 1024))" "$v"
  rm -f "$out" "$out.stdout"
done
