"""T4 (Octane scores, every engine and no-JIT mode) and T5 (instructions,
cycles, CPU time, peak RSS of the deterministic `-run.js`, a fixed amount of
work), one run at a time. Appends rows to octane.tsv:
engine, bench, metric, value.

    python3 octane.py [engines,...] [bench ...]
"""
import os, re, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eng import ENGINES, run, abc, ROOT, OUT, TMP

OCT = os.path.join(ROOT, "target", "octane")
BENCHES = ("richards deltablue crypto raytrace earley-boyer regexp splay navier-stokes "
           "pdfjs mandreel gbemu code-load box2d zlib typescript").split()
def wrapper(engine, path):
    """A script printing `path`'s completion value (as cloison prints a script's):
    a copy whose last line, the completion, is passed to `print`; for ARK
    compiled to `.abc`, with the harness's `alert` declared (`es2abc`
    compiles every script as strict mode code)."""
    if engine == "cloison" or engine == "cloison-nojit":
        return path
    w = f"{TMP}/wrap/{os.path.basename(path)}"
    os.makedirs(f"{TMP}/wrap", exist_ok=True)
    lines = open(path).read().rstrip("\n").split("\n")
    open(w, "w").write("\n".join(lines[:-1] + [f"print({lines[-1].rstrip(';')});"]) + "\n")
    if engine.startswith("ark"):
        return abc(w, "var alert;\n") or w
    return w


def emit(e, b, k, v):
    out = OUT
    out.write(f"{e}\t{b}\t{k}\t{v}\n")
    out.flush()



if __name__ == "__main__":
    engines = sys.argv[1].split(",") if len(sys.argv) > 1 and sys.argv[1] != "all" else list(ENGINES)
    benches = sys.argv[2:] or BENCHES
    OUT = open(os.environ.get("OCTANE_TSV", f"{OUT}/octane.tsv"), "a")

    for b in benches:
        for e in engines:
            # T4: the score.
            r = run(e, wrapper(e, f"{OCT}/{b}-score.js"), timeout=3600)
            last = (r.stdout.strip().splitlines() or [""])[-1]
            score = last if re.fullmatch(r"[\d.]+", last) else None
            emit(e, b, "score", score or "-")
            # T5: counters for the fixed work.
            r = run(e, wrapper(e, f"{OCT}/{b}-run.js"), timeout=3600,
                    wrap=["/usr/bin/time", "-f", "TIME %e %M", "perf", "stat", "-x,", "-e",
                          "instructions:u,cycles:u,task-clock"])
            ok = any(l.strip() in ("1001", "1002") for l in r.stdout.splitlines())
            if not ok:
                emit(e, b, "run_failed", (r.stdout + r.stderr).strip()[-200:].replace("\n", " ").replace("\t", " "))
                print(f"{e} {b}: score {score} run FAILED", file=sys.stderr, flush=True)
                continue
            m = {k: float(v) for v, k in re.findall(r"^([\d.]+),[^,]*,([\w:-]+)", r.stderr, re.M)}
            wall, rss = re.search(r"TIME ([\d.]+) (\d+)", r.stderr).groups()
            emit(e, b, "instructions", int(m["instructions:u"]))
            emit(e, b, "cycles", int(m["cycles:u"]))
            emit(e, b, "cpu_ms", m["task-clock"])
            emit(e, b, "wall_s", wall)
            emit(e, b, "rss_mb", round(int(rss) / 1024, 1))
            print(f"{e} {b}: score {score} instr {m['instructions:u'] / 1e9:.2f}G wall {wall}s rss {int(rss) // 1024}MB",
                  file=sys.stderr, flush=True)
