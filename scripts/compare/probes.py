"""M7 (memory given back), M8 (limits) and F3 (realms), from probe scripts.
Appends to probes.tsv: engine, probe, metric, value.

  M7  RSS before, at a peak holding ~400 MB of small arrays, and after
      dropping them and full collections (at once, and 0.5 s and 2 s later:
      sweeping and returning memory may be concurrent or delayed)
  M8  the deepest recursion a default stack allows, the longest string
      (doubled until a RangeError)
  F3  N realms made: microseconds and KB each (cloison and JavaScriptCore
      `$262.createRealm()`, V8 `Realm.create()`, SpiderMonkey `newGlobal()`;
      QuickJS's shell has none)
"""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import run_probe
from eng import OUT

M7 = """
var keep = [];
mark("before");
for (var i = 0; i < 1600000; i++) keep.push([i, i, i, i, i, i, i, i, i, i, i, i, i, i, i, i, i, i, i, i]);
mark("peak");
keep = null;
mark("after");
spin(500); mark("after_0_5s");
spin(1500); mark("after_2s");
print(0);
"""

M8 = """
var depth = 0;
function down() { depth++; down(); }
try { down(); } catch (e) {}
print("depth " + depth);
var s = "x", n = 1;
try { for (;;) { var t = s + s; t.charCodeAt(t.length - 1); s = t; n = s.length; } } catch (e) {}
print("string " + n);
"""

REALM = {"cloison": "$262.createRealm()", "jsc": "$262.createRealm()", "v8": "Realm.create()", "sm": "newGlobal()"}


def f3(e):
    return f"""
var N = 1000, keep = new Array(N);
for (var i = 0; i < N; i++) keep[i] = 0;
mark("base");
var t = Date.now();
for (var i = 0; i < N; i++) keep[i] = {REALM[e]};
var ms = Date.now() - t;
mark("full");
print("us " + (ms * 1000 / N));
"""


if __name__ == "__main__":
    engines = sys.argv[1].split(",") if len(sys.argv) > 1 else ["cloison", "v8", "jsc", "sm", "qjs"]
    out = open(f"{OUT}/probes.tsv", "a")

    def emit(e, p, k, v):
        out.write(f"{e}\t{p}\t{k}\t{v}\n")
        out.flush()
        print(e, p, k, v, flush=True)

    for e in engines:
        marks, _ = run_probe(e, M7)
        for k, v in marks.items():
            emit(e, "m7", k + "_mb", round(v / 1024, 1) if v else "-")
        _, lines = run_probe(e, M8)
        for l in lines:
            w = l.split()
            if len(w) == 2 and w[0] in ("depth", "string"):
                emit(e, "m8", w[0], w[1])
        if e in REALM:
            marks, lines = run_probe(e, f3(e))
            us = [l.split()[1] for l in lines if l.startswith("us ")]
            if us and marks.get("full") and marks.get("base"):
                emit(e, "f3", "us_per_realm", us[0])
                emit(e, "f3", "kb_per_realm", round((marks["full"] - marks["base"]) / 1000, 1))
