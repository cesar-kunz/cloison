#!/usr/bin/env bash
# What each Octane benchmark costs besides its score: in toi, the bytecode
# loaded, the machine code the JIT generated, compilations and their time,
# the run's time and peak RSS (`TOI_SIZES`); the same where other engines
# tell it: V8 (Node 26) the bytecode and code it holds at the end, Node's
# own subtracted, and its Maglev and Turbofan compile times
# (`--trace-opt`, which leaves Sparkplug out); JavaScriptCore its total
# compile time; SpiderMonkey and QuickJS time and RSS only. And each
# engine's runtime: the code (`.text`) of its binary and libraries.
#
#   scripts/octane-sizes.sh                  # every benchmark
#   scripts/octane-sizes.sh zlib typescript  # only these
#   scripts/octane-sizes.sh --record         # also append toi's to bench/octane-sizes.tsv
#
# Each runs the deterministic `-run.js` once (sizes don't vary, times do:
# REPS=n takes medians), JOBS (default 8) at a time. Tables go to
# perf-history/octane-sizes-<commit>.md (and .tsv). With --record, toi's
# values are appended to bench/octane-sizes.tsv (`commit<TAB>bench<TAB>
# metric<TAB>value`, `runtime` for the binary) and compared with the last
# commit recorded there.
#
# Engines: $NODE, else the Node 26 scripts/tiers.sh downloads; the others
# from ~/.jsvu (`npx jsvu --os=linux64
# --engines=javascriptcore,spidermonkey,quickjs`), left out if missing.
# Only toi runs every time: the other engines' values are kept in
# bench/octane-sizes-engines.tsv by engine version, and measured
# only when missing (a new version, a new benchmark) or with ENGINES=1.
# OTHERS=0 leaves out all but toi and V8.
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
NODE=${NODE:-$(ls -d ~/.local/share/toi-v8/node-*/bin/node 2>/dev/null | tail -1)}
[[ -x $NODE ]] || { echo "no Node 26: run scripts/tiers.sh once, or set NODE" >&2; exit 1; }
cargo build --release --quiet --bin toi
# The `-run.js` scripts.
scripts/octane.sh $order >/dev/null
commit=$(git rev-parse --short HEAD)
git diff --quiet HEAD -- src stencils build.rs .cargo || commit="$commit+"
mkdir -p perf-history
python3 - "$NODE" "$commit" "$record" "${REPS:-1}" "${JOBS:-8}" "${OTHERS:-1}" "${ENGINES:-0}" $order <<'EOF'
import concurrent.futures as cf, json, os, re, statistics, subprocess, sys, tempfile

node, commit, record, reps, jobs, others, remeasure, *benches = sys.argv[1:]
record, reps, jobs, remeasure = record == "true", int(reps), int(jobs), remeasure == "1"
jsvu = os.path.expanduser("~/.jsvu/bin")
tmp = tempfile.mkdtemp()
try:
    jsvu_versions = json.load(open(os.path.expanduser("~/.jsvu/status.json")))
except OSError:
    jsvu_versions = {}

def wrapper(engine, path):
    """A script printing `path`'s completion value, as toi does."""
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

engines = {
    "toi": lambda f: (["target/release/toi", f], {"TOI_SIZES": "1"}),
    "V8": lambda f: ([node, "--trace-opt", "scripts/v8sizes.js", f], {}),
}
if others == "1":
    for e, extra in [("javascriptcore", ["--reportTotalCompileTimes=true"]), ("spidermonkey", []), ("quickjs", [])]:
        if os.path.exists(os.path.join(jsvu, e)):
            engines[e] = (lambda e, extra: lambda f: ([os.path.join(jsvu, e)] + extra + [wrapper(e, f)], {}))(e, extra)
names = {"javascriptcore": "JSC", "spidermonkey": "SpiderMonkey", "quickjs": "QuickJS"}

def run(job):
    e, b = job
    f = os.path.abspath(f"target/octane/{b}-run.js") if b != "<empty>" else os.path.join(tmp, "empty.js")
    argv, env = engines[e](f)
    timing = os.path.join(tmp, f"time-{e}-{b}-{os.getpid()}-{id(job)}")
    r = subprocess.run(["/usr/bin/time", "-o", timing, "-f", "%e %M"] + argv, capture_output=True,
                       text=True, env={**os.environ, **env}, timeout=1800)
    wall, rss = open(timing).read().split()[-2:]
    # (The result is the last line toi prints; JSC's compile times follow it.)
    ok = any(l.strip() in ("1001", "1002") for l in r.stdout.splitlines())
    m = {"s": float(wall), "rss_mb": int(rss) / 1024, "ok": ok or b == "<empty>"}
    err = r.stderr
    if e == "toi":
        for k, v in re.findall(r"(\w+)=([\d.]+)", err.split("sizes ", 1)[-1].splitlines()[0] if "sizes " in err else ""):
            m[k] = float(v)
    elif e == "V8":
        line = [l for l in err.splitlines() if l.startswith("v8sizes ")]
        for k, v in re.findall(r"(\w+)=([\d.]+)", line[0] if line else ""):
            m["v8_" + k] = float(v)
        # Each optimizing compile: prepare, execute, finalize ms.
        m["opt_ms"] = sum(sum(map(float, t)) for t in re.findall(r"took ([\d.]+), ([\d.]+), ([\d.]+) ms", r.stdout + err))
        m["opt_compiles"] = len(re.findall(r"\[completed compiling", r.stdout + err))
    elif e == "javascriptcore":
        t = re.search(r"Total Compile Time: ([\d.]+) ms", r.stdout + err)
        m["compile_ms"] = float(t.group(1)) if t else None
    return job, m

def version(e):
    if e == "V8":
        return subprocess.run([node, "-p", "process.versions.v8"], capture_output=True, text=True).stdout.strip()
    return jsvu_versions.get(e, "?")
versions = {e: version(e) for e in engines if e != "toi"}

# The other engines' values measured before, by engine version.
cache_path = "bench/octane-sizes-engines.tsv"
cache = {}
if os.path.exists(cache_path):
    for l in open(cache_path).read().splitlines()[1:]:
        e, ver, b, k, v = l.split("\t")
        if versions.get(e) == ver:
            cache.setdefault((e, b), {})[k] = None if v == "-" else float(v)
results = {}
for (e, b), m in cache.items():
    if not remeasure and (b in benches or b == "<empty>"):
        results[(e, b)] = [m]

open(os.path.join(tmp, "empty.js"), "w").write("0\n")
work = [(e, b) for b in benches + ["<empty>"] for e in engines
        if (e == "toi" and b != "<empty>") or (e == "V8" and b == "<empty>" and (e, b) not in results)
        or (e != "toi" and b != "<empty>" and (e, b) not in results)]
work = [j for j in work for _ in range(reps)]
measured = set()
with cf.ThreadPoolExecutor(jobs) as ex:
    for n, (job, m) in enumerate(ex.map(run, work), 1):
        if job[0] != "toi" and job not in measured:
            measured.add(job)
            results[job] = []
        results.setdefault(job, []).append(m)
        print(f"\r{n}/{len(work)} runs", end="", file=sys.stderr, flush=True)
print(file=sys.stderr)
# Keep the new ones (medians), and every other cached row.
if measured:
    keep = []
    if os.path.exists(cache_path):
        for l in open(cache_path).read().splitlines()[1:]:
            e, ver, b, *_ = l.split("\t")
            if (e, b) not in measured or versions.get(e) != ver:
                keep.append(l)
    for e, b in sorted(measured):
        ms = results[(e, b)]
        if not all(m["ok"] for m in ms):
            continue
        for k in sorted({k for m in ms for k in m if k != "ok"}):
            v = [m.get(k) for m in ms if m.get(k) is not None]
            keep.append(f"{e}\t{versions[e]}\t{b}\t{k}\t{statistics.median(v) if v else '-'}")
    with open(cache_path, "w") as f:
        f.write("engine\tversion\tbench\tmetric\tvalue\n" + "".join(l + "\n" for l in keep))

def val(e, b, k):
    # (A run that didn't print its result counts for nothing.)
    ms = results.get((e, b), [])
    if not all(m.get("ok", True) for m in ms):
        return None
    v = [m.get(k) for m in ms if m.get(k) is not None]
    return statistics.median(v) if v and len(v) == len(ms) else None

for (e, b), ms in results.items():
    if not all(m.get("ok", True) for m in ms):
        print(f"{b} ({e}): did not print its result", file=sys.stderr)

base_bc, base_code = val("V8", "<empty>", "v8_bytecode"), val("V8", "<empty>", "v8_code")
kb = lambda v: None if v is None else v / 1024
cols = [
    ("toi functions", lambda b: val("toi", b, "functions")),
    ("toi bytecode KB", lambda b: kb(val("toi", b, "bytecode"))),
    ("toi code KB (hot)", lambda b: kb(val("toi", b, "jit_hot"))),
    ("toi code KB", lambda b: kb(val("toi", b, "jit_total"))),
    ("toi compiles", lambda b: val("toi", b, "compiles")),
    ("toi compile ms", lambda b: val("toi", b, "compile_ms")),
    ("toi lower ms", lambda b: val("toi", b, "lower_ms")),
    ("toi s", lambda b: val("toi", b, "s")),
    ("toi RSS MB", lambda b: val("toi", b, "rss_mb")),
    ("V8 bytecode KB", lambda b: kb(None if val("V8", b, "v8_bytecode") is None else val("V8", b, "v8_bytecode") - base_bc)),
    ("V8 code KB", lambda b: kb(None if val("V8", b, "v8_code") is None else val("V8", b, "v8_code") - base_code)),
    ("V8 opt compiles", lambda b: val("V8", b, "opt_compiles")),
    ("V8 opt compile ms", lambda b: val("V8", b, "opt_ms")),
    ("V8 s", lambda b: val("V8", b, "s")),
    ("V8 RSS MB", lambda b: val("V8", b, "rss_mb")),
]
def ratio(a, b):
    return None if a is None or not b else a / b
cols += [
    ("toi/V8 code", lambda b: ratio(val("toi", b, "jit_total"), None if val("V8", b, "v8_code") is None else val("V8", b, "v8_code") - base_code)),
    ("toi/V8 bytecode", lambda b: ratio(val("toi", b, "bytecode"), None if val("V8", b, "v8_bytecode") is None else val("V8", b, "v8_bytecode") - base_bc)),
    ("toi/V8 compile ms", lambda b: ratio(val("toi", b, "compile_ms"), val("V8", b, "opt_ms"))),
    ("toi/V8 s", lambda b: ratio(val("toi", b, "s"), val("V8", b, "s"))),
    ("toi/V8 RSS", lambda b: ratio(val("toi", b, "rss_mb"), val("V8", b, "rss_mb"))),
]
if "javascriptcore" in engines:
    cols += [("toi/JSC compile ms", lambda b: ratio(val("toi", b, "compile_ms"), val("javascriptcore", b, "compile_ms"))),
             ("toi/JSC s", lambda b: ratio(val("toi", b, "s"), val("javascriptcore", b, "s")))]
if "javascriptcore" in engines:
    cols += [("JSC compile ms", lambda b: val("javascriptcore", b, "compile_ms")),
             ("JSC s", lambda b: val("javascriptcore", b, "s")),
             ("JSC RSS MB", lambda b: val("javascriptcore", b, "rss_mb"))]
for e in ("spidermonkey", "quickjs"):
    if e in engines:
        cols += [(f"{names[e]} s", lambda b, e=e: val(e, b, "s")),
                 (f"{names[e]} RSS MB", lambda b, e=e: val(e, b, "rss_mb"))]

def fmt(v):
    if v is None:
        return "-"
    return f"{v:,.0f}" if abs(v) >= 100 else f"{v:,.1f}" if abs(v) >= 10 else f"{v:,.2f}"

# Runtimes: the code in each engine's binary and its own libraries.
def text(paths):
    t = 0
    for p in paths:
        r = subprocess.run(["size", "-A", p], capture_output=True, text=True).stdout
        t += sum(int(l.split()[1]) for l in r.splitlines() if l.split()[:1] == [".text"])
    return t
def binaries(e):
    # (~/.jsvu/bin holds wrapper scripts; the engines and the libraries
    # they ship are under ~/.jsvu/engines/<engine>.)
    d = os.path.expanduser(f"~/.jsvu/engines/{e}")
    files = [os.path.join(d, f) for f in os.listdir(d)] + [os.path.join(d, "lib", f) for f in os.listdir(os.path.join(d, "lib"))] if os.path.isdir(os.path.join(d, "lib")) else [os.path.join(d, f) for f in os.listdir(d)]
    return [f for f in files if os.path.isfile(f) and open(f, "rb").read(4) == b"\x7fELF"]
runtimes = [("toi", ["target/release/toi"]), ("V8 (node)", [os.path.realpath(node)])]
runtimes += [(names[e], binaries(e)) for e in engines if e in names]
runtime_text = [(n, text(p)) for n, p in runtimes]

out = f"perf-history/octane-sizes-{commit}"
v8 = subprocess.run([node, "-p", "process.versions.v8"], capture_output=True, text=True).stdout.strip()
with open(out + ".tsv", "w") as f:
    f.write("bench\t" + "\t".join(c[0] for c in cols) + "\n")
    for b in benches:
        f.write(b + "\t" + "\t".join("-" if (v := c[1](b)) is None else f"{v:.6g}" for c in cols) + "\n")
others_desc = ", ".join(f"{names[e]} {versions[e]}" for e in engines if e in names) or "no other engines"
lines = [f"Octane sizes and compile costs: toi {commit}, V8 {v8} (live at the end, Node's own subtracted; "
         f"opt: Maglev and Turbofan, prepare+execute+finalize), {others_desc} (other engines' values "
         f"cached by version in {cache_path}). "
         f"Deterministic `-run.js` runs, median of {reps}.", ""]
for title, group in [("toi", [c for c in cols if c[0].startswith("toi ")]),
                     ("toi against V8 and JSC (ratios; below 1: toi uses less)", [c for c in cols if c[0].startswith("toi/")]),
                     ("other engines", [c for c in cols if not c[0].startswith("toi")])]:
    lines += [f"**{title}**", "", "| benchmark | " + " | ".join(c[0] for c in group) + " |",
              "|---|" + "---:|" * len(group)]
    for b in benches:
        lines.append(f"| {b} | " + " | ".join(fmt(c[1](b)) for c in group) + " |")
    lines.append("")
lines += ["**runtime** (`.text` of the binary and its own libraries)", "", "| engine | KB |", "|---|---:|"]
lines += [f"| {n} | {t / 1024:,.0f} |" for n, t in runtime_text]
open(out + ".md", "w").write("\n".join(lines) + "\n")
print("\n".join(lines))

if record:
    path = "bench/octane-sizes.tsv"
    rows = [l.rstrip("\n").split("\t") for l in open(path)][1:] if os.path.exists(path) else []
    last = {}
    prev = next((r[0] for r in reversed(rows) if r[0] != commit), None)
    for r in rows:
        if r[0] == prev:
            last[(r[1], r[2])] = float(r[3])
    keys = ["functions", "bytecode", "jit_hot", "jit_total", "compiles", "compile_ms"]
    new = [(b, k, val("toi", b, k)) for b in benches for k in keys if val("toi", b, k) is not None]
    new += [("runtime", "text", runtime_text[0][1])]
    with open(path, "a") as f:
        if not rows:
            f.write("commit\tbench\tmetric\tvalue\n")
        for b, k, v in new:
            f.write(f"{commit}\t{b}\t{k}\t{v:.6g}\n")
    if prev:
        print(f"\nChanges since {prev} (past 2%):")
        for b, k, v in new:
            o = last.get((b, k))
            if o and abs(v - o) > 0.02 * o and k != "compile_ms":
                print(f"  {b} {k}: {o:,.0f} -> {v:,.0f} ({100 * (v - o) / o:+.1f}%)")
EOF
