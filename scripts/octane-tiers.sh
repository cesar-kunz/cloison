#!/usr/bin/env bash
# Octane's score for each of its benchmarks in each of toi's and V8's
# tiers: toi's interpreter alone (TOI_NO_JIT) and with the JIT; V8 capped
# at each tier (`--max-opt`): Ignition (0), Sparkplug (1), Maglev (2), and
# everything (Turbofan, V8's default). Each score is Octane's own (its
# harness and files unmodified: scripts/octane.sh's `-score.js` scripts),
# the median of REPS (default 3) runs; higher is better. The table goes to
# perf-history/octane-tiers-<commit>.md (and .tsv), with geometric means
# over the benchmarks every column scored.
#
#   scripts/octane-tiers.sh                   # every benchmark
#   scripts/octane-tiers.sh deltablue splay   # only these
#
# Runs go JOBS (default 8) at a time, each pinned to a core of its own,
# the idlest ones (see scripts/quiet.sh), and to both of its CPUs: toi's
# marker and Node's compiler and collector run on threads of their own.
# Parallel runs share the L3 cache and memory bandwidth. A run past
# TIMEOUT seconds (default 900) or giving no score counts as `-`.
#
# Node: $NODE, else the Node 26 scripts/tiers.sh downloads.
set -euo pipefail
cd "$(dirname "$0")/.."
reps=${REPS:-3}
jobs=${JOBS:-8}
timeout=${TIMEOUT:-900}
NODE=${NODE:-$(ls -d ~/.local/share/toi-v8/node-*/bin/node 2>/dev/null | tail -1)}
[[ -x $NODE ]] || { echo "no Node 26: run scripts/tiers.sh once, or set NODE" >&2; exit 1; }
order="richards deltablue crypto raytrace earley-boyer regexp splay
  navier-stokes pdfjs mandreel gbemu code-load box2d zlib typescript"
[[ $# -gt 0 ]] && order="$*"
# The `-score.js` scripts (and the toi build).
scripts/octane.sh $order >/dev/null
commit=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- src stencils build.rs .cargo || commit="$commit+"
mkdir -p perf-history
out=perf-history/octane-tiers-$commit

python3 - "$NODE" "$reps" "$jobs" "$timeout" "$out" "$commit" $order <<'EOF'
import concurrent.futures as cf, math, os, queue, statistics, subprocess, sys, time

node, reps, jobs, timeout, out, commit, *benches = sys.argv[1:]
reps, jobs, timeout = int(reps), int(jobs), int(timeout)
v8 = subprocess.run([node, "-p", "process.versions.v8"], capture_output=True, text=True).stdout.strip()
runner = 'console.log(String(require("vm").runInThisContext(require("fs").readFileSync(process.argv[1], "utf8"))))'
configs = [
    ("toi interp", lambda f: (["target/release/toi", f], {"TOI_NO_JIT": "1"})),
    ("toi JIT", lambda f: (["target/release/toi", f], {})),
    ("Ignition", lambda f: ([node, "--max-opt=0", "-e", runner, f], {})),
    ("Sparkplug", lambda f: ([node, "--max-opt=1", "-e", runner, f], {})),
    ("Maglev", lambda f: ([node, "--max-opt=2", "-e", runner, f], {})),
    ("Turbofan", lambda f: ([node, "-e", runner, f], {})),
]

def siblings(c):
    s = open(f"/sys/devices/system/cpu/cpu{c}/topology/thread_siblings_list").read().strip()
    r = []
    for p in s.split(","):
        lo, _, hi = p.partition("-")
        r += range(int(lo), int(hi or lo) + 1)
    return tuple(r)

def idle_cores(n):
    def snap():
        r = {}
        for l in open("/proc/stat"):
            if l.startswith("cpu") and l[3].isdigit():
                f = l.split(); v = list(map(int, f[1:]))
                r[int(f[0][3:])] = (v[3] + v[4], sum(v))
        return r
    a = snap(); time.sleep(0.5); b = snap()
    busy = {c: 1 - (b[c][0] - a[c][0]) / max(1, b[c][1] - a[c][1]) for c in a}
    cores = {siblings(c) for c in busy} - {siblings(0)}
    return sorted(cores, key=lambda s: sum(busy[c] for c in s))[:n]

pool = queue.Queue()
for c in idle_cores(jobs):
    pool.put(c)

def run(job):
    b, k, _ = job
    name, cmd = configs[k]
    argv, env = cmd(f"target/octane/{b}-score.js")
    core = pool.get()
    try:
        cpus = ",".join(map(str, core))
        try:
            r = subprocess.run(["taskset", "-c", cpus] + argv, capture_output=True, text=True,
                               timeout=timeout, env={**os.environ, **env})
            last = (r.stdout.strip().splitlines() or [""])[-1]
        except subprocess.TimeoutExpired:
            last = "timed out"
    finally:
        pool.put(core)
    try:
        return job, float(last)
    except ValueError:
        print(f"{b} ({name}): {last[:120]}", file=sys.stderr)
        return job, None

# Longest first (the interpreter's), so the end isn't one long run alone.
work = [(b, k, i) for k in range(len(configs)) for b in benches for i in range(reps)]
scores = {}
load = [open("/proc/loadavg").read().split()[0]]
with cf.ThreadPoolExecutor(jobs) as ex:
    for n, (job, s) in enumerate(ex.map(run, work), 1):
        scores.setdefault(job[:2], []).append(s)
        print(f"\r{n}/{len(work)} runs", end="", file=sys.stderr, flush=True)
print(file=sys.stderr)
load.append(open("/proc/loadavg").read().split()[0])

def score(b, k):
    v = [s for s in scores[(b, k)] if s is not None]
    return statistics.median(v) if len(v) == len(scores[(b, k)]) else None

names = [c[0] for c in configs]
table = {b: [score(b, k) for k in range(len(configs))] for b in benches}
with open(out + ".tsv", "w") as f:
    f.write("bench\t" + "\t".join(names) + "\n")
    for b in benches:
        f.write(b + "\t" + "\t".join("-" if s is None else f"{s:g}" for s in table[b]) + "\n")

fmt = lambda s: "-" if s is None else f"{s:.0f}" if s >= 100 else f"{s:.3g}"
lines = [f"Octane scores (higher is better; medians of {reps} runs, {jobs} at a time on cores of their own): "
         f"toi {commit}, V8 {v8} capped at each tier. Load average at the start and end: {' '.join(load)}.\n",
         "| benchmark | " + " | ".join(names) + " |", "|---|" + "---:|" * len(names)]
for b in benches:
    lines.append(f"| {b} | " + " | ".join(fmt(s) for s in table[b]) + " |")
ok = [b for b in benches if all(s is not None for s in table[b])]
if ok:
    gm = [math.exp(sum(math.log(table[b][k]) for b in ok) / len(ok)) for k in range(len(configs))]
    lines.append(f"| **geomean** ({len(ok)}) | " + " | ".join(f"**{fmt(g)}**" for g in gm) + " |")
    lines.append("")
    for k in (0, 1):
        lines.append(f"{names[k]} vs " + ", ".join(
            f"{names[j]} {gm[k] / gm[j]:.2f}x (ahead on {sum(table[b][k] > table[b][j] for b in ok)})"
            for j in range(2, len(configs))) + f" (of {len(ok)}; above 1x: toi scores higher)")
missing = [b for b in benches if b not in ok]
if missing:
    lines.append("")
    lines.append("Not in the geomean (a column without a score): " + ", ".join(missing) + ".")
open(out + ".md", "w").write("\n".join(lines) + "\n")
print("\n".join(lines))
EOF
