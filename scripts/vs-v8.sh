#!/usr/bin/env bash
# Time every script in bench/micro/ in cloison and in V8 (Node), each run a
# fresh process, and compare.
#
#   scripts/vs-v8.sh            # measure both; table in perf-history/vs-v8-<commit>.tsv
#   scripts/vs-v8.sh --cloison      # re-measure cloison only, reusing the newest V8 times
#
# V8: `perf-history/v8time.js` runs the script with real globals
# (`vm.runInThisContext`) and times it from compile to completion, Node's
# own startup excluded. cloison: CPU time of the whole process (`perf stat`
# task-clock), startup included. Each the median of REPS runs (the
# machine's timings can be bimodal; a mean would land between the modes),
# pinned to an idle core (scripts/quiet.sh): cloison to one of its CPUs, Node
# (whose compiler and collector run on threads of their own) to the core's
# CPUs (both SMT siblings).
# Results are checked against each script's `// expect:` line. The
# summary is cloison's speed relative to V8 (above 1: cloison is faster).
set -euo pipefail
cd "$(dirname "$0")/.."
reps=${REPS:-7}
hist=perf-history
mkdir -p "$hist"
[[ -f $hist/v8time.js ]] || cat >"$hist/v8time.js" <<'EOF'
// Time one script in V8's main context (real globals), from compile to completion.
const fs = require("fs"), vm = require("vm");
const src = fs.readFileSync(process.argv[2], "utf8");
const t0 = process.hrtime.bigint();
const v = vm.runInThisContext(src);
const t1 = process.hrtime.bigint();
process.stdout.write(`${Number(t1 - t0) / 1e6} ${String(v)}\n`);
EOF
cloison_only=false
[[ ${1:-} == --cloison ]] && cloison_only=true
cargo build --release --quiet --bin cloison
commit=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- src stencils build.rs .cargo || commit="$commit+"
out=$hist/vs-v8-$commit.tsv
prev=$(ls -t "$hist"/vs-v8-*.tsv 2>/dev/null | grep -v "/vs-v8-$commit.tsv" | head -1 || true)
node=$(node --version)
tmp=$(mktemp)
printf "commit\tnode\tbench\tv8_ms\ttoi_ms\n" >"$tmp"
for f in bench/micro/*.js; do
  b=$(basename "$f" .js)
  core=$(scripts/quiet.sh --core)
  want=$(sed -n 's|^// expect: ||p' "$f")
  if $cloison_only && [[ -n $prev ]]; then
    v8=$(awk -F'\t' -v b="$b" '$3 == b { print $4 }' "$prev")
  else
    v8=$(for _ in $(seq "$reps"); do taskset -c "$core" node "$hist/v8time.js" "$f"; done |
      awk -v w="$want" '$2 != w { print "V8 gave " $2 > "/dev/stderr" } { print $1 }' |
      sort -g | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }')
  fi
  got=$(target/release/cloison "$f" 2>&1 || true)
  if [[ $got != "$want" ]]; then
    echo "cloison gave '$got' for $b, expected '$want'" >&2
    continue
  fi
  cloison=$(for _ in $(seq "$reps"); do QUIET_CPU=${core%%,*} scripts/quiet.sh perf stat -x, -e task-clock target/release/cloison "$f" 2>&1 >/dev/null | cut -d, -f1; done |
    sort -g | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }')
  printf "%s\t%s\t%s\t%s\t%s\n" "$commit" "$node" "$b" "$v8" "$cloison" >>"$tmp"
done
mv "$tmp" "$out"
python3 - "$out" "$prev" <<'EOF'
import math, sys
def load(p):
    r = {}
    for l in list(open(p))[1:]:
        _, _, b, v, t = l.rstrip("\n").split("\t")
        r[b] = float(v) / float(t)
    return r
new = load(sys.argv[1])
old = load(sys.argv[2]) if sys.argv[2] else {}
gm = lambda d: math.exp(sum(map(math.log, d.values())) / len(d))
print(f"{sys.argv[1]}: geomean {gm(new):.2f}x, cloison faster on {sum(r > 1 for r in new.values())} of {len(new)}"
      + (f" (was {gm(old):.2f}x, {sum(r > 1 for r in old.values())})" if old else ""))
for b in sorted(new, key=new.get):
    if new[b] < 1:
        was = f"  (was {old[b]:.2f}x)" if b in old else ""
        print(f"  {b:34} {new[b]:.2f}x{was}")
EOF
