#!/usr/bin/env bash
# Time every script in bench/micro/ in each of toi's and V8's tiers: toi's
# interpreter alone (TOI_NO_JIT) and with the JIT; V8 capped at each tier
# (`--max-opt`): Ignition (0), Sparkplug (1), Maglev (2), and everything
# (Turbofan, V8's default). The table goes to perf-history/tiers-<commit>.md
# (and .tsv), with geometric means at the bottom.
#
#   scripts/tiers.sh            # measure toi; V8 only where not cached
#   scripts/tiers.sh --v8       # re-measure V8 too
#
# V8's times don't change with toi, so they're cached in
# perf-history/v8-tiers.tsv by host, Node version, tier, and the
# benchmark's contents (`git hash-object`): a new or modified benchmark is
# measured, the rest reused. Delete the cache (or pass --v8) after changing
# how V8 is timed.
#
# Timing as in scripts/vs-v8.sh: V8 from compile to completion inside the
# process (perf-history/v8time.js, Node's startup excluded); toi the CPU time
# of the whole process (about 0.4 ms of it startup). Medians of REPS (toi,
# default 7) and V8REPS (default 11) runs, pinned to an idle core
# (scripts/quiet.sh). Results are checked against `// expect:`.
#
# Node: $NODE, else a Node 26 (V8 14.6) the script downloads to
# ~/.local/share/toi-v8 (the system's Node may predate Maglev).
set -euo pipefail
cd "$(dirname "$0")/.."
reps=${REPS:-7}
v8reps=${V8REPS:-11}
hist=perf-history
cache=$hist/v8-tiers.tsv
host=$(hostname)
mkdir -p "$hist"

node_ver=v26.8.2
if [[ -z ${NODE:-} ]]; then
  dir=$HOME/.local/share/toi-v8
  NODE=$dir/node-$node_ver-linux-x64/bin/node
  if [[ ! -x $NODE ]]; then
    f=node-$node_ver-linux-x64.tar.xz
    mkdir -p "$dir"
    (cd "$dir" && curl -sfO "https://nodejs.org/dist/$node_ver/$f" &&
      curl -sf "https://nodejs.org/dist/$node_ver/SHASUMS256.txt" | grep " $f\$" | sha256sum -c --quiet - &&
      tar xf "$f" && rm "$f")
  fi
fi
v8=$("$NODE" -p 'process.versions.v8')

[[ -f $hist/v8time.js ]] || cat >"$hist/v8time.js" <<'EOF'
// Time one script in V8's main context (real globals), from compile to completion.
const fs = require("fs"), vm = require("vm");
const src = fs.readFileSync(process.argv[2], "utf8");
const t0 = process.hrtime.bigint();
const v = vm.runInThisContext(src);
const t1 = process.hrtime.bigint();
process.stdout.write(`${Number(t1 - t0) / 1e6} ${String(v)}\n`);
EOF
[[ -f $cache ]] || printf 'host\tv8\ttier\tbench\tsha\tms\treps\n' >"$cache"
[[ ${1:-} == --v8 ]] && remeasure=true || remeasure=false

cargo build --release --quiet --bin toi
commit=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- src stencils build.rs .cargo || commit="$commit+"

median() { sort -g | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'; }

# v8_ms TIER FLAGS FILE WANT CORE
v8_ms() {
  local i
  for i in $(seq "$v8reps"); do
    taskset -c "$5" "$NODE" $2 "$hist/v8time.js" "$3"
  done | awk -v w="$4" -v t="$1" -v f="$3" '$2 != w { print "V8 (" t ") gave " $2 " for " f > "/dev/stderr" } { print $1 }' | median
}

# toi_ms FILE CORE [ENV...]
toi_ms() {
  local f=$1 core=$2 i
  shift 2
  for i in $(seq "$reps"); do
    env "$@" QUIET_CPU=${core%%,*} scripts/quiet.sh perf stat -x, -e task-clock target/release/toi "$f" 2>&1 >/dev/null | cut -d, -f1
  done | median
}

tiers=(ignition sparkplug maglev turbofan)
declare -A flags=([ignition]=--max-opt=0 [sparkplug]=--max-opt=1 [maglev]=--max-opt=2 [turbofan]=)
out=$hist/tiers-$commit.tsv
tmp=$(mktemp)
printf 'bench\ttoi_interp\ttoi_jit\t%s\n' "$(IFS=$'\t'; echo "${tiers[*]}")" >"$tmp"
for f in bench/micro/*.js; do
  b=$(basename "$f" .js)
  want=$(sed -n 's|^// expect: ||p' "$f")
  sha=$(git hash-object "$f")
  core=$(scripts/quiet.sh --core)
  row=()
  for e in TOI_NO_JIT=1 ""; do
    e=($e)
    got=$(env "${e[@]}" target/release/toi "$f" 2>&1 || true)
    if [[ $got == "$want" ]]; then row+=("$(toi_ms "$f" "$core" "${e[@]}")"); else
      echo "toi (${e[*]:-JIT}) gave '$got' for $b, expected '$want'" >&2
      row+=(-)
    fi
  done
  for t in "${tiers[@]}"; do
    ms=
    $remeasure || ms=$(awk -F'\t' -v h="$host" -v v="$v8" -v t="$t" -v s="$sha" -v b="$b" \
      '$1 == h && $2 == v && $3 == t && $4 == b && $5 == s { m = $6 } END { print m }' "$cache")
    if [[ -z $ms ]]; then
      ms=$(v8_ms "$t" "${flags[$t]}" "$f" "$want" "$core")
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$host" "$v8" "$t" "$b" "$sha" "$ms" "$v8reps" >>"$cache"
    fi
    row+=("$ms")
  done
  printf '%s\t%s\n' "$b" "$(IFS=$'\t'; echo "${row[*]}")" >>"$tmp"
done
mv "$tmp" "$out"

python3 - "$out" "$commit" "$v8" <<'EOF' | tee "${out%.tsv}.md"
import math, sys
path, commit, v8 = sys.argv[1:]
rows = [l.rstrip("\n").split("\t") for l in open(path)]
head, rows = rows[0], rows[1:]
names = ["toi interp", "toi JIT", "Ignition", "Sparkplug", "Maglev", "Turbofan"]
fmt = lambda x: "-" if x is None else f"{x:.3g}" if x < 100 else f"{x:.0f}"
val = lambda s: None if s == "-" else float(s)
print(f"Time (ms, medians) per benchmark: toi {commit}, V8 {v8} capped at each tier.\n")
print("| benchmark | " + " | ".join(names) + " |")
print("|---|" + "---:|" * len(names))
cols = [[] for _ in names]
for r in rows:
    vs = [val(s) for s in r[1:]]
    for c, v in zip(cols, vs):
        c.append(v)
    print(f"| {r[0]} | " + " | ".join(fmt(v) for v in vs) + " |")
# Geometric means over the benchmarks every column ran.
ok = [i for i in range(len(rows)) if all(c[i] is not None for c in cols)]
gm = [math.exp(sum(math.log(c[i]) for i in ok) / len(ok)) for c in cols]
print(f"| **geomean** ({len(ok)}) | " + " | ".join(f"**{fmt(g)}**" for g in gm) + " |")
print()
for k, who in ((0, "toi interp"), (1, "toi JIT")):
    print(f"{who} vs " + ", ".join(
        f"{names[j]} {gm[j] / gm[k]:.2f}x (faster on {sum(cols[k][i] < cols[j][i] for i in ok)})"
        for j in range(2, 6)) + f" (of {len(ok)}; above 1x: toi faster)")
EOF
