"""L2 (loading a script: parsing and compiling to bytecode), L3 (JIT
compile time) and L4 (deoptimizations). Appends to compile.tsv: engine,
bench, metric, value.

L2: each Octane benchmark's source in a function never called, so the
engine parses (and compiles what it compiles eagerly) and runs nothing:
instructions and CPU time over an empty script's (perf stat, medians of 5),
per MB of source. Every engine parses inner functions lazily.

L3 and L4, over the fixed work (the deterministic `-run.js`), each engine
by its own flags:
  cloison  CLOISON_SIZES's compile_ms + lower_ms and compiles; deopts, the
           exits from compiled code CLOISON_EXITS counts (not entries,
           `enter`, nor runtime calls, `rt_call`)
  v8       --trace-opt: each Maglev or Turbofan compilation's prepare,
           execute and finalize ms (Sparkplug's not traced); --trace-deopt:
           its bailouts
  jsc      --reportTotalCompileTimes (every tier, Baseline to FTL);
           --printEachOSRExit: its speculation failures
  sm, qjs  not traced by a release shell (SpiderMonkey), no JIT (QuickJS)

    python3 compile.py [l2|l3] [engines] [bench...]
"""
import os, re, statistics, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eng import run, OUT, TMP, ENGINES
from octane import wrapper, OCT, BENCHES


def perf(e, f, n=5):
    r = run(e, f, wrap=["perf", "stat", "-r", str(n), "-x,", "-e", "instructions:u,task-clock"], timeout=600)
    instr = float(re.search(r"^([\d.]+),,instructions:u", r.stderr, re.M).group(1))
    cpu = float(re.search(r"^([\d.]+),msec,task-clock", r.stderr, re.M).group(1))
    return instr, cpu


def l2(engines, benches, out):
    open(f"{TMP}/empty.js", "w").write("0\n")
    base = {e: perf(e, f"{TMP}/empty.js", 20) for e in engines}
    for b in benches:
        src = open(f"{OCT}/{b}-run.js").read()
        f = f"{TMP}/load-{b}.js"
        open(f, "w").write("(function () {\n" + src + "\n});\n")
        mb = len(src.encode()) / 2**20
        for e in engines:
            instr, cpu = perf(e, f)
            i, c = (instr - base[e][0]) / 1e6 / mb, (cpu - base[e][1]) / mb
            out.write(f"{e}\t{b}\tload_Minstr_per_mb\t{i:.2f}\n{e}\t{b}\tload_ms_per_mb\t{c:.3f}\n")
            out.flush()
            print(f"{e} {b} ({mb:.2f} MB): {i:.1f} M instructions/MB, {c:.2f} ms/MB", file=sys.stderr, flush=True)


def l3(engines, benches, out):
    for b in benches:
        for e in engines:
            f = wrapper(e, f"{OCT}/{b}-run.js")
            m = {}
            if e == "cloison":
                r = run(e, f, env={"CLOISON_SIZES": "1"}, timeout=3600)
                s = dict(re.findall(r"(\w+)=([\d.]+)", re.search(r"^sizes .*$", r.stderr, re.M).group(0)))
                m["jit_ms"] = float(s["compile_ms"]) + float(s["lower_ms"])
                m["jit_compiles"] = float(s["compiles"])
                r = run(e, f, env={"CLOISON_EXITS": "1"}, timeout=3600)
                m["deopts"] = sum(int(l.split()[0]) for l in r.stderr.splitlines()
                                  if re.match(r"\s*\d+\s", l) and "enter @" not in l and "rt_call" not in l)
            elif e == "v8":
                r = run(e, f, args=["--trace-opt", "--trace-deopt"], timeout=3600)
                o = r.stdout + r.stderr
                m["jit_ms"] = sum(sum(map(float, t)) for t in re.findall(r"took ([\d.]+), ([\d.]+), ([\d.]+) ms", o))
                m["jit_compiles"] = len(re.findall(r"^\[completed compiling", o, re.M))
                m["deopts"] = len(re.findall(r"^\[bailout \(kind: deopt", o, re.M))
            elif e == "jsc":
                r = run(e, f, args=["--reportTotalCompileTimes=1"], timeout=3600)
                t = re.search(r"Total Compile Time: ([\d.]+) ms", r.stdout + r.stderr)
                m["jit_ms"] = float(t.group(1)) if t else 0.0
                r = run(e, f, args=["--printEachOSRExit=1"], timeout=3600)
                m["deopts"] = len(re.findall(r"^Speculation failure", r.stdout + r.stderr, re.M))
            for k, v in m.items():
                out.write(f"{e}\t{b}\t{k}\t{v}\n")
            out.flush()
            print(e, b, m, file=sys.stderr, flush=True)


if __name__ == "__main__":
    what = sys.argv[1]
    engines = sys.argv[2].split(",") if len(sys.argv) > 2 else (
        ["cloison", "v8", "jsc", "sm", "qjs"] if what == "l2" else ["cloison", "v8", "jsc"])
    benches = sys.argv[3:] or BENCHES
    out = open(f"{OUT}/compile.tsv", "a")
    (l2 if what == "l2" else l3)(engines, benches, out)
