#!/usr/bin/env bash
# Benchmark timings to track across master's history: cycles and
# instructions (user space, `perf stat`, mean of REPS runs) for every
# bytecode listing in bench/bytecode/ and every script in bench/micro/ the
# build runs correctly. Recorded rows go to perf-history/perf.tsv, which is
# untracked: timings belong to a machine, so each row names its host.
#
#   scripts/perf.sh [--ab [REV]]    # the working tree against REV (default
#                                   # HEAD) built in a worktree, their runs
#                                   # interleaved (A B A B …) so load on the
#                                   # machine affects both alike; the change
#                                   # is the median of the paired ratios, the
#                                   # spread the middle half of them (a change
#                                   # within it is noise)
#   scripts/perf.sh --record        # measure the working tree, compare with
#                                   # the newest recorded ancestor of HEAD
#                                   # (noisier: another session's load), and
#                                   # record it (HEAD's commit, `+` if src/,
#                                   # stencils/, bench/ or build.rs differ)
#   scripts/perf.sh --backfill [N]  # build and record the last N (default all)
#                                   # first-parent commits of master not yet
#                                   # recorded on this host, each in a worktree
#   scripts/perf.sh --history [RE] [K]
#                                   # cycles (millions) of benchmarks matching
#                                   # RE over the last K (default 8) recorded
#                                   # master commits, oldest first
#
# Runs aren't pinned to a CPU: on this shared host a pinned run can't leave
# a CPU another process lands on (see scripts/quiet.sh). REPS (default 11)
# sets the runs per benchmark. A listing is named
# `asm/NAME` and a script `js/NAME`; a benchmark's code may change between
# commits (listings follow the code generator).
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
hist=perf-history
tsv=$hist/perf.tsv
host=$(hostname)
reps=${REPS:-11}
mkdir -p "$hist"
[[ -f $tsv ]] || printf 'commit\tdate\thost\tbench\tcycles\tcycles_sd\tinstructions\treps\n' >"$tsv"

# measure DIR BIN: one `bench<TAB>cycles<TAB>sd<TAB>instructions` line per
# benchmark of checkout DIR run with BIN.
measure() {
  local dir=$1 bin=$2 f expect out
  for f in "$dir"/bench/bytecode/*.asm "$dir"/bench/micro/*.js; do
    [[ -f $f ]] || continue
    local name=$(basename "$f") kind=asm
    [[ $f == *.js ]] && kind=js
    local js="$dir/bench/micro/${name%.*}.js"
    expect=$(sed -n 's|^// expect: ||p' "$js" 2>/dev/null | head -1)
    out=$(timeout 60 "$bin" "$f" 2>/dev/null) || continue
    [[ $out == "$expect" ]] || continue
    perf stat -x, -e cycles:u,instructions:u -r "$reps" "$bin" "$f" 2>&1 >/dev/null |
      awk -F, -v b="$kind/${name%.*}" '
        $3 == "cycles:u" { c = $1; sd = $4 }
        $3 == "instructions:u" { i = $1 }
        END { sub(/%/, "", sd); printf "%s\t%s\t%s\t%s\n", b, c, sd, i }'
  done
}

# record COMMIT ROWS
record() {
  local commit=$1 date
  date=$(git log -1 --format=%cI "${commit%+}" 2>/dev/null || echo "")
  awk -F'\t' -v c="$commit" -v d="$date" -v h="$host" -v r="$reps" \
    'NF { printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n", c, d, h, $1, $2, $3, $4, r }' <<<"$2" >>"$tsv"
}

recorded() { awk -F'\t' -v c="$1" -v h="$host" '$1 == c && $3 == h { found = 1 } END { exit !found }' "$tsv"; }

case ${1:-} in
--backfill)
  work=$hist/work
  mapfile -t commits < <(git rev-list --first-parent ${2:+--max-count=$2} master)
  for full in "${commits[@]}"; do
    c=$(git rev-parse --short "$full")
    recorded "$c" && continue
    echo "== $c $(git log -1 --format=%s "$c")"
    rm -rf "$work/tree"
    git worktree prune
    git worktree add -q --detach "$work/tree" "$c"
    if (cd "$work/tree" && CARGO_TARGET_DIR="$root/$work/target" cargo build --release --quiet --bin cloison 2>/dev/null); then
      rows=$(measure "$work/tree" "$root/$work/target/release/cloison")
      if [[ -n $rows ]]; then record "$c" "$rows"; echo "recorded $(wc -l <<<"$rows") benchmarks"; else echo "nothing ran"; fi
    else
      echo "build failed; skipped"
    fi
    git worktree remove --force "$work/tree"
  done
  exit 0
  ;;
--history)
  mapfile -t order < <(git rev-list --first-parent --reverse master | cut -c1-7)
  python3 - "$tsv" "$host" "${2:-.}" "${3:-8}" "${order[@]}" <<'EOF'
import re, sys
tsv, host, pat, k, order = sys.argv[1], sys.argv[2], re.compile(sys.argv[3]), int(sys.argv[4]), sys.argv[5:]
data = {}
for line in list(open(tsv))[1:]:
    c, _, h, b, cyc, *_ = line.rstrip("\n").split("\t")
    if h == host and pat.search(b):
        data.setdefault(b, {})[c] = float(cyc) / 1e6
seen = {c for v in data.values() for c in v}
cols = [c for c in order if c in seen][-k:]
w = max([len(b) for b in data] + [5])
print(f"{'cycles (M)':{w}} " + " ".join(f"{c:>8}" for c in cols))
for b in sorted(data):
    print(f"{b:{w}} " + " ".join(f"{data[b][c]:8.2f}" if c in data[b] else f"{'':>8}" for c in cols))
EOF
  exit 0
  ;;
--ab | "")
  rev=$(git rev-parse --short "${2:-HEAD}")
  work=$hist/ab
  rm -rf "$work/tree"
  git worktree prune
  git worktree add -q --detach "$work/tree" "$rev"
  (cd "$work/tree" && CARGO_TARGET_DIR="$root/$work/target" cargo build --release --quiet --bin cloison)
  git worktree remove --force "$work/tree"
  cargo build --release --quiet --bin cloison
  a=$work/target/release/cloison b=target/release/cloison
  printf '%-36s %10s %10s %8s %8s %8s\n' bench "$rev" "working" change spread insns
  for f in bench/bytecode/*.asm bench/micro/*.js; do
    name=$(basename "$f") kind=asm
    [[ $f == *.js ]] && kind=js
    expect=$(sed -n 's|^// expect: ||p' "bench/micro/${name%.*}.js" 2>/dev/null | head -1)
    [[ $(timeout 60 "$a" "$f" 2>/dev/null) == "$expect" ]] || continue
    [[ $(timeout 60 "$b" "$f" 2>/dev/null) == "$expect" ]] || continue
    for _ in $(seq "$reps"); do
      for bin in "$a" "$b"; do
        perf stat -x, -e cycles:u,instructions:u "$bin" "$f" 2>&1 >/dev/null |
          awk -F, '$3 == "cycles:u" { c = $1 } $3 == "instructions:u" { i = $1 } END { printf "%s %s ", c, i }'
      done
      echo
    done | python3 -c '
import statistics as st, sys
r = [list(map(float, l.split())) for l in sys.stdin if l.strip()]
ca, ia, cb, ib = zip(*r)
ratio = sorted(b / a for a, b in zip(ca, cb))
# The spread: the middle half of the paired ratios.
q = lambda p: ratio[min(len(ratio) - 1, int(p * len(ratio)))]
di = st.median(ib) / st.median(ia) - 1
print(f"{sys.argv[1]:36} {st.median(ca) / 1e6:9.2f}M {st.median(cb) / 1e6:9.2f}M {100 * (st.median(ratio) - 1):+7.1f}% {100 * (q(.75) - q(.25)):7.1f}% {100 * di:+7.1f}%")
' "$kind/${name%.*}"
  done
  exit 0
  ;;
--record) ;;
*) sed -n '2,25p' "$0"; exit 2 ;;
esac

cargo build --release --quiet --bin cloison
commit=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- src stencils bench build.rs Cargo.toml || commit="$commit+"
rows=$(measure . target/release/cloison)

# The newest recorded first-parent ancestor of HEAD on this host.
base=""
for c in $(git rev-list --first-parent HEAD | cut -c1-7); do
  if recorded "$c"; then base=$c; break; fi
done
printf '%-36s %10s %8s %10s %10s %8s\n' "bench ($commit)" cycles sd% insns "${base:-—}" change
while IFS=$'\t' read -r b cyc sd ins; do
  before=""
  [[ -n $base ]] && before=$(awk -F'\t' -v c="$base" -v h="$host" -v b="$b" '$1 == c && $3 == h && $4 == b { print $5 }' "$tsv" | tail -1)
  change=""
  [[ -n $before ]] && change=$(awk -v a="$before" -v b="$cyc" 'BEGIN { printf "%+.1f%%", 100 * (b - a) / a }')
  awk -v b="$b" -v c="$cyc" -v sd="$sd" -v i="$ins" -v p="$before" -v ch="$change" 'BEGIN {
    printf "%-36s %9.2fM %8s %9.2fM %10s %8s\n", b, c / 1e6, sd, i / 1e6, p == "" ? "" : sprintf("%.2fM", p / 1e6), ch }'
done <<<"$rows"
if [[ ${1:-} == --record ]]; then
  record "$commit" "$rows"
  echo "recorded $commit in $tsv"
fi
