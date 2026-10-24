#!/bin/bash
# Sweep the sparse-array settings (docs/sparse-arrays.md, "Choosing the
# defaults") over the workloads here: for each setting D,D',W,P and each
# workload, the median time of REPS runs and the peak RSS, as a TSV.
#
#   bench/sparse/sweep.sh [--reps R] [--no-wait] [--points FILE] [--engines] BIN OUT.tsv
#
# --engines adds V8, SpiderMonkey and JSC (~/.jsvu/bin) as settings of their
# own. Unless --no-wait, each run waits for a 1-minute load below 4.
# bench/sparse/pareto.py summarizes OUT.tsv.
set -e
reps=3 wait=1 points= engines=
while [ $# -gt 2 ]; do
  case $1 in
    --reps) reps=$2; shift 2 ;;
    --no-wait) wait=; shift ;;
    --points) points=$2; shift 2 ;;
    --engines) engines=1; shift ;;
    *) break ;;
  esac
done
bin=$(realpath "$1") out=$2
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -r -- "$tmp"' EXIT

# The settings: always dense, far at any gap, then the grid.
if [ -n "$points" ]; then
  mapfile -t settings < "$points"
else
  settings=("0,0,inf,inf" "1,1,0,0")
  for d in 1/2 1/4 1/8 1/16 1/32; do
    for k in 2 4; do
      b=$(python3 -c "from fractions import Fraction as F; print(min(F('$d') * $k, 1))")
      for w in 8k 64k 256k 1m; do
        settings+=("$d,$b,$w,16m")
      done
    done
  done
  for p in 1m 128m; do settings+=("1/8,1/2,8k,$p"); done
fi
[ -n "$engines" ] && settings+=(v8 sm jsc)

# Workload, then the prelude setting its sizes.
runs=(
  "forward N=1e6"
  "prealloc N=1e4" "prealloc N=1e5" "prealloc N=1e6" "prealloc N=1e7"
  "reverse N=1e4" "reverse N=1e6"
  "random N=1e4" "random N=1e6"
  "strided N=1e5;K=2" "strided N=1e5;K=4" "strided N=1e5;K=8" "strided N=1e5;K=16" "strided N=1e5;K=64" "strided N=1e4;K=1024"
  "prefix-far N=1e5"
  "id-map N=1e5"
  "many N=2000"
  "methods N=1e5;K=4" "methods N=1e5;K=16"
)

wait_load() {
  [ -n "$wait" ] || return 0
  until awk '{exit !($1 < 4)}' /proc/loadavg; do sleep 30; done
}

[ -s "$out" ] || printf 'setting\tworkload\tsizes\tms\trss_kb\tchecksum\n' > "$out"
for s in "${settings[@]}"; do
  for r in "${runs[@]}"; do
    w=${r%% *} sizes=${r#* }
    # (Resumable: a row already there is kept.)
    grep -qP "^\Q$s\E\t$w\t\Q$sizes\E\t" "$out" && continue
    f=$tmp/$w.js
    { echo "var ${sizes//;/; var };"; cat "$here/$w.js"; } > "$f"
    case $s in
      v8|sm|jsc) cmd=(~/.jsvu/bin/$s "$f") ;;
      *) cmd=("$bin" --array-density "$s" "$f") ;;
    esac
    ms=() rss=0 sum=
    for ((i = 0; i < reps; i++)); do
      wait_load
      line=$( { /usr/bin/time -f 'RSS %M' timeout 120 "${cmd[@]}" 2>&1 || echo 'TIMEOUT'; } | grep -v '^undefined$' | tr '\n' ' ')
      if [[ $line == *TIMEOUT* ]]; then ms+=(120000); rss=-1; sum=timeout; break; fi
      read -r _ _ t c <<< "${line%%RSS*}"
      k=${line##*RSS }; k=${k%% *}
      ms+=("$t"); sum=$c
      (( k > rss )) && rss=$k
    done
    med=$(printf '%s\n' "${ms[@]}" | sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}')
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$s" "$w" "$sizes" "$med" "$rss" "$sum" >> "$out"
    printf '%-22s %-10s %-12s %8s ms %10s KB\n' "$s" "$w" "$sizes" "$med" "$rss"
  done
done
