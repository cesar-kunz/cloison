#!/usr/bin/env bash
# The memory each Octane benchmark takes in cloison and in the other engines:
# the process's peak RSS and its mean RSS over the run (sampled every 10
# ms, the process and its children), each engine's empty script's alongside,
# what is above it being what the benchmark took. And cloison's
# collector (`CLOISON_SIZES`): pauses, minor and full, their total and
# longest; the heap's peak (bytes of blocks in use) and the most a full
# collection found live: their ratio is the heap's overhead, its room to
# grow and its fragmentation, what compaction would bring down.
#
#   scripts/octane-memory.sh                  # every benchmark
#   scripts/octane-memory.sh splay pdfjs      # only these
#   scripts/octane-memory.sh --record         # also append cloison's to bench/octane-memory.tsv
#
# Each runs the deterministic `-run.js` (the same work in every engine),
# the median of REPS (default 3) runs, JOBS (default 8) at a time. Tables
# go to perf-history/octane-memory-<commit>.md (and .tsv). With --record,
# cloison's values are appended to bench/octane-memory.tsv
# (`commit<TAB>bench<TAB>metric<TAB>value`) and compared with the last
# commit recorded there.
#
# Engines: V8 (Node, unflagged: Turbofan) as $NODE, else the Node 26
# scripts/tiers.sh downloads; JavaScriptCore, SpiderMonkey and QuickJS from
# ~/.jsvu (`npx jsvu --os=linux64 --engines=javascriptcore,spidermonkey,quickjs`),
# left out if missing. Only cloison runs every time: the other engines'
# values are kept in bench/octane-memory-engines.tsv by engine version, and
# measured only when missing (a new version, a new benchmark) or with
# ENGINES=1. Linux only (/proc).
set -euo pipefail
cd "$(dirname "$0")/.."
record=false
if [[ ${1:-} == --record ]]; then
  record=true
  shift
fi
order="richards deltablue crypto raytrace earley-boyer regexp splay
  navier-stokes pdfjs mandreel gbemu code-load box2d zlib typescript"
[[ $# -gt 0 ]] && order="$*"
NODE=${NODE:-$(ls -d ~/.local/share/cloison-v8/node-*/bin/node 2>/dev/null | tail -1 || true)}
[[ -x $NODE ]] || { echo "no Node 26: run scripts/tiers.sh once, or set NODE" >&2; exit 1; }
# The `-run.js` scripts (and the cloison build).
scripts/octane.sh $order >/dev/null
commit=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- src stencils build.rs .cargo || commit="$commit+"
mkdir -p perf-history
python3 - "$NODE" "$commit" "$record" "${REPS:-3}" "${JOBS:-8}" "${ENGINES:-0}" $order <<'EOF'
import concurrent.futures as cf, glob, json, math, os, re, statistics, subprocess, sys, tempfile, time

node, commit, record, reps, jobs, remeasure, *benches = sys.argv[1:]
record, reps, jobs, remeasure = record == "true", int(reps), int(jobs), remeasure == "1"
jsvu = os.path.expanduser("~/.jsvu/bin")
tmp = tempfile.mkdtemp()
try:
    jsvu_versions = json.load(open(os.path.expanduser("~/.jsvu/status.json")))
except OSError:
    jsvu_versions = {}

def wrapper(engine, path):
    """A script printing `path`'s completion value, as cloison does."""
    w = os.path.join(tmp, f"{engine}-{os.path.basename(path)}")
    if engine == "quickjs":
        w += ".mjs"
        # (`read` as the other shells have it: zlib's Emscripten code calls it.)
        src = (f'import * as std from "qjs:std";\nglobalThis.read = std.loadFile;\n'
               f'print((0, eval)(std.loadFile({path!r})));\n')
    else:
        src = f"print((0, eval)(read({path!r})));\n"
    open(w, "w").write(src)
    return w

runner = 'console.log(String(require("vm").runInThisContext(require("fs").readFileSync(process.argv[1], "utf8"))))'
engines = {
    "cloison": lambda f: (["target/release/cloison", f], {"CLOISON_SIZES": "1"}),
    "V8": lambda f: ([node, "-e", runner, f], {}),
}
for e in ("javascriptcore", "spidermonkey", "quickjs"):
    if os.path.exists(os.path.join(jsvu, e)):
        engines[e] = (lambda e: lambda f: ([os.path.join(jsvu, e), wrapper(e, f)], {}))(e)
names = {"cloison": "cloison", "V8": "V8", "javascriptcore": "JSC", "spidermonkey": "SpiderMonkey", "quickjs": "QuickJS"}

def tree_rss(pid):
    """The resident KiB of `pid`'s descendants (0 once gone)."""
    kb, todo = 0, [pid]
    while todo:
        p = todo.pop()
        try:
            if p != pid:
                for l in open(f"/proc/{p}/status"):
                    if l.startswith("VmRSS:"):
                        kb += int(l.split()[1])
            for c in glob.glob(f"/proc/{p}/task/*/children"):
                todo += map(int, open(c).read().split())
        except (OSError, ValueError):
            pass
    return kb

def run(job):
    e, b, _ = job
    f = os.path.abspath(f"target/octane/{b}-run.js") if b != "<empty>" else os.path.join(tmp, "empty.js")
    argv, env = engines[e](f)
    out, err = tempfile.TemporaryFile("w+"), tempfile.TemporaryFile("w+")
    # The peak from `time`: a child of this process would count the
    # memory it was forked with, Python's, before its exec.
    peak_file = os.path.join(tmp, f"peak-{e}-{b}-{job[2]}")
    t0 = time.monotonic()
    p = subprocess.Popen(["/usr/bin/time", "-f", "%M", "-o", peak_file] + argv, stdout=out, stderr=err,
                         env={**os.environ, **env})
    samples = []
    while p.poll() is None:
        samples.append(tree_rss(p.pid))
        if time.monotonic() - t0 > 1800:
            p.kill()
        time.sleep(0.01)
    s = time.monotonic() - t0
    out.seek(0); err.seek(0)
    stdout, stderr = out.read(), err.read()
    # (`time`'s peak covers the children the process waited for: a shell
    # wrapper's engine.)
    peak = max([int(open(peak_file).read().split()[-1])] + samples) / 1024
    m = {"s": s, "peak_mb": peak, "mean_mb": statistics.mean(samples) / 1024 if samples else peak,
         "ok": b == "<empty>" or any(l.strip() in ("1001", "1002") for l in stdout.splitlines())}
    if e == "cloison":
        line = next((l for l in stderr.splitlines() if l.startswith("gc ")), "")
        for k, v in re.findall(r"(\w+)=([\d.]+)", line):
            m[k] = float(v)
    return job, m

def version(e):
    if e == "V8":
        return subprocess.run([node, "-p", "process.versions.v8"], capture_output=True, text=True).stdout.strip()
    return jsvu_versions.get(e, "?")
versions = {e: version(e) for e in engines if e != "cloison"}

# The other engines' values measured before, by engine version.
cache_path = "bench/octane-memory-engines.tsv"
results = {}
if os.path.exists(cache_path) and not remeasure:
    for l in open(cache_path).read().splitlines()[1:]:
        e, ver, b, k, v = l.split("\t")
        if versions.get(e) == ver and (b in benches or b == "<empty>"):
            results.setdefault((e, b), [{"ok": True}])[0][k] = float(v)

open(os.path.join(tmp, "empty.js"), "w").write("0\n")
work = [(e, b, i) for b in benches + ["<empty>"] for e in engines
        if e == "cloison" or (e, b) not in results for i in range(reps)]
# Longest first, so the end isn't one long run alone.
work.sort(key=lambda j: (j[1] == "<empty>", j[0] != "quickjs"))
measured = set()
with cf.ThreadPoolExecutor(jobs) as ex:
    for n, (job, m) in enumerate(ex.map(run, work), 1):
        results.setdefault(job[:2], [])
        if job[0] != "cloison":
            measured.add(job[:2])
        results[job[:2]].append(m)
        print(f"\r{n}/{len(work)} runs", end="", file=sys.stderr, flush=True)
print(file=sys.stderr)
for (e, b), ms in results.items():
    if not all(m["ok"] for m in ms):
        print(f"{b} ({names[e]}): did not print its result", file=sys.stderr)

def val(e, b, k):
    # (A run that didn't print its result counts for nothing.)
    ms = results.get((e, b), [])
    if not ms or not all(m["ok"] for m in ms):
        return None
    v = [m[k] for m in ms if k in m]
    return statistics.median(v) if len(v) == len(ms) else None

# Keep the new ones (medians), and every other cached row.
if measured:
    keep = []
    if os.path.exists(cache_path):
        for l in open(cache_path).read().splitlines()[1:]:
            e, ver, b, *_ = l.split("\t")
            if (e, b) not in measured or versions.get(e) != ver:
                keep.append(l)
    for e, b in sorted(measured):
        for k in ("peak_mb", "mean_mb", "s"):
            if (v := val(e, b, k)) is not None:
                keep.append(f"{e}\t{versions[e]}\t{b}\t{k}\t{v:.6g}")
    with open(cache_path, "w") as f:
        f.write("engine\tversion\tbench\tmetric\tvalue\n" + "".join(l + "\n" for l in keep))

def above(e, b, k):
    """What the benchmark took above the engine's empty script's peak, at
    least 1 MB. (Its mean is no baseline: it ends before most samples.)"""
    v, base = val(e, b, k), val(e, "<empty>", "peak_mb")
    return None if v is None or base is None else max(1.0, v - base)

def fmt(v):
    if v is None:
        return "-"
    return f"{v:,.0f}" if abs(v) >= 100 else f"{v:,.1f}" if abs(v) >= 10 else f"{v:,.2f}"
def gm(vs):
    return math.exp(sum(map(math.log, vs)) / len(vs)) if vs else None

es = list(engines)
v8 = versions["V8"]
lines = [f"Octane memory (deterministic `-run.js`, median of {reps}): cloison {commit}, "
         + ", ".join(f"{names[e]} {versions[e]}" for e in es if e != "cloison")
         + f" (other engines' values cached by version in {cache_path}). Peak and mean RSS of the "
         "process and its children (sampled every 10 ms), MB; `above empty`: less the engine's empty "
         "script's peak, at least 1.", ""]
tsv = ["bench\t" + "\t".join(f"{names[e]} {k}" for k in ("peak_mb", "mean_mb") for e in es)]
for b in benches + ["<empty>"]:
    tsv.append(b + "\t" + "\t".join("-" if (v := val(e, b, k)) is None else f"{v:.6g}"
                                    for k in ("peak_mb", "mean_mb") for e in es))
for k, title in (("peak_mb", "peak RSS"), ("mean_mb", "mean RSS")):
    lines += [f"**{title}, MB** (in parentheses: above empty)", "",
              "| benchmark | " + " | ".join(names[e] for e in es) + " |", "|---|" + "---:|" * len(es)]
    for b in benches:
        lines.append(f"| {b} | " + " | ".join(
            "-" if val(e, b, k) is None else f"{fmt(val(e, b, k))} ({fmt(above(e, b, k))})" for e in es) + " |")
    if k == "peak_mb":
        lines.append("| *empty script* | " + " | ".join(fmt(val(e, "<empty>", k)) for e in es) + " |")
    # Geomeans over the benchmarks every engine ran, and cloison against each.
    ok = [b for b in benches if all(above(e, b, k) is not None for e in es)]
    if ok:
        g = {e: gm([above(e, b, k) for b in ok]) for e in es}
        lines.append(f"| **geomean above empty** ({len(ok)}) | " + " | ".join(f"**{fmt(g[e])}**" for e in es) + " |")
        lines += ["", "cloison against " + ", ".join(
            f"{names[e]} {g['cloison'] / g[e]:.2f}x (less on {sum(above('cloison', b, k) < above(e, b, k) for b in ok)})"
            for e in es if e != "cloison") + f" (above empty, of {len(ok)}; below 1x: cloison takes less)"]
    lines.append("")

gc_cols = [("minors", "minors", None), ("fulls", "fulls", None), ("pause_ms", "pauses ms", None),
           ("max_pause_ms", "longest ms", None), ("heap_peak", "heap peak MB", 1 << 20),
           ("live_peak", "live peak MB", 1 << 20)]
lines += ["**cloison's collector** (heap: bytes of blocks in use at its peak; live: the most a full "
          "collection found live)", "",
          "| benchmark | " + " | ".join(c[1] for c in gc_cols) + " | heap/live |", "|---|" + "---:|" * (len(gc_cols) + 1)]
for b in benches:
    row = [val("cloison", b, k) for k, _, _ in gc_cols]
    row = [None if v is None else v / d if d else v for v, (_, _, d) in zip(row, gc_cols)]
    h, l = val("cloison", b, "heap_peak"), val("cloison", b, "live_peak")
    lines.append(f"| {b} | " + " | ".join(f"{v:.0f}" if v is not None and k in ("minors", "fulls") else fmt(v)
                                          for v, (k, _, _) in zip(row, gc_cols)) + f" | {fmt(h / l) if h and l else '-'} |")
# (Over the benchmarks that keep at least a quarter of the nursery live:
# below, the nursery is most of the heap's peak.)
big = [b for b in benches if (val("cloison", b, "live_peak") or 0) >= 8 << 20 and val("cloison", b, "heap_peak")]
if big:
    g = gm([val("cloison", b, "heap_peak") / val("cloison", b, "live_peak") for b in big])
    lines.append(f"| **geomean** ({', '.join(big)}) | " + " |" * len(gc_cols) + f" **{g:.2f}** |")
for b in benches:
    tsv[benches.index(b) + 1] += "\t" + "\t".join("-" if (v := val("cloison", b, k)) is None else f"{v:.6g}" for k, _, _ in gc_cols)
tsv[0] += "\t" + "\t".join(f"cloison {k}" for k, _, _ in gc_cols)

out = f"perf-history/octane-memory-{commit}"
open(out + ".tsv", "w").write("\n".join(tsv) + "\n")
open(out + ".md", "w").write("\n".join(lines) + "\n")
print("\n".join(lines))

if record:
    path = "bench/octane-memory.tsv"
    rows = [l.rstrip("\n").split("\t") for l in open(path)][1:] if os.path.exists(path) else []
    prev = next((r[0] for r in reversed(rows) if r[0] != commit), None)
    last = {(r[1], r[2]): float(r[3]) for r in rows if r[0] == prev}
    keys = ["peak_mb", "mean_mb", "heap_peak", "live_peak", "minors", "fulls", "pause_ms", "max_pause_ms"]
    new = [(b, k, val("cloison", b, k)) for b in benches + ["<empty>"] for k in keys if val("cloison", b, k) is not None]
    with open(path, "a") as f:
        if not rows:
            f.write("commit\tbench\tmetric\tvalue\n")
        for b, k, v in new:
            f.write(f"{commit}\t{b}\t{k}\t{v:.6g}\n")
    if prev:
        # (Memory past 5%; pause times vary too much from run to run.)
        print(f"\nChanges since {prev} (past 5%):")
        for b, k, v in new:
            o = last.get((b, k))
            if o and abs(v - o) > 0.05 * o and "pause" not in k:
                print(f"  {b} {k}: {o:,.6g} -> {v:,.6g} ({100 * (v - o) / o:+.1f}%)")
EOF
