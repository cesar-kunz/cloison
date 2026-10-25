"""M3: bytes per object of each kind, from the RSS of N kept alive (after
full collections) less the RSS with the holder alone; at N and 2N, the
slope between them (fixed costs and heap rounding cancel) is the figure.
Appends to m3.tsv: engine, kind, metric, value."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import re
from probe import run_probe, prelude
from eng import ENGINES, OUT, TMP, run
KINDS = {
    "empty {}": "{}",
    "{a,b,c} literal": "{a: i, b: i, c: i}",
    "ctor, 3 fields": "new P3(i)",
    "{10 props} literal": "{a: i, b: i, c: i, d: i, e: i, f: i, g: i, h: i, j: i, k: i}",
    "array [i,i,i]": "[i, i, i]",
    "array of 3 doubles": "[i + 0.5, i + 1.5, i + 2.5]",
    "closure": "mk(i)",
    "string, 16 chars": "('abcdefgh' + (10000000 + i)).slice(0, 16) + ''",
    "heap double": "i + 0.5",
    "Map entry": None,   # (a Map of N entries, int keys)
    "array element (int)": None,  # (an array of N ints, the holder left out)
}
SETUP = """
function P3(x) { this.a = x; this.b = x; this.c = x; }
function mk(x) { return function () { return x; }; }
"""


def body(kind, n):
    expr = KINDS[kind]
    if kind == "Map entry":
        return SETUP + f"""var N = {n}; var m = new Map();
mark("base");
for (var i = 0; i < N; i++) m.set(i, i);
mark("full");
print(m.size);
"""
    if kind == "array element (int)":
        return SETUP + f"""var N = {n}; var a;
mark("base");
a = []; for (var i = 0; i < N; i++) a.push(i);
mark("full");
print(a.length);
"""
    return SETUP + f"""var N = {n}, keep = new Array(N);
for (var i = 0; i < N; i++) keep[i] = 0;
mark("base");
for (var i = 0; i < N; i++) keep[i] = {expr};
mark("full");
print(keep.length);
"""


def qjs_allocated(src):
    """QuickJS's own count of the bytes malloc holds for it (`-d`, at exit):
    its allocator reuses what the holder's growth freed, which RSS misses."""
    f = f"{TMP}/m3-qjs.mjs"
    open(f, "w").write(prelude("qjs").replace("print(\"@\" + name); spin(", "(") + src)
    r = run("qjs", f, args=["-d"], timeout=600)
    return int(re.search(r"memory allocated\s+\d+\s+(\d+)", r.stdout + r.stderr).group(1))


def qjs_bytes(k, n):
    """Per object: N of them less the holder alone (its N zeros)."""
    holder = body("array element (int)", n) if k == "array element (int)" else body(k, n).replace(
        f"keep[i] = {KINDS[k]};", "keep[i] = 0;") if KINDS[k] else None
    full = qjs_allocated(body(k, n))
    if k == "Map entry":
        empty = qjs_allocated(body(k, n).replace("m.set(i, i);", ""))
    elif k == "array element (int)":
        empty = qjs_allocated(body(k, n).replace("a.push(i);", ""))
    else:
        empty = qjs_allocated(holder)
    return full - empty


if __name__ == "__main__":
    engines = sys.argv[1].split(",") if len(sys.argv) > 1 else ["cloison", "v8", "jsc", "sm", "qjs"]
    kinds = sys.argv[2:] or list(KINDS)
    out = open(f"{OUT}/m3.tsv", "a")
    N = 500000
    for k in kinds:
        row = []
        for e in engines:
            d = {}
            for n in (N, 2 * N):
                if e == "qjs":
                    d[n] = qjs_bytes(k, n)
                    continue
                marks, lines = run_probe(e, body(k, n))
                if "base" in marks and "full" in marks and marks["base"] and marks["full"]:
                    d[n] = (marks["full"] - marks["base"]) * 1024
            if len(d) == 2:
                slope = (d[2 * N] - d[N]) / N
                at2n = d[2 * N] / (2 * N)
                out.write(f"{e}\t{k}\tbytes\t{slope:.1f}\n{e}\t{k}\tbytes_at_2n\t{at2n:.1f}\n")
                out.flush()
                row.append(f"{e} {slope:.0f} ({at2n:.0f})")
            else:
                row.append(f"{e} failed")
        print(f"{k:22s} " + "  ".join(row), flush=True)
