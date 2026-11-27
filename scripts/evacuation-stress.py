#!/usr/bin/env python3
"""Run tests/scripts with every collection full and moving cells, one at
each safepoint (`CLOISON_GC_FULL=1 CLOISON_GC_EVACUATE=all
CLOISON_GC_STRESS=1`), in the default `pin` mode: a cell whose address
something keeps that the collector doesn't update (a `Vm` table, compiled
code, a copy the native stack's scan doesn't reach) goes stale where it
moves, and a value rooted nowhere is freed by the first full collection.
The scripts' harness (tests/scripts.rs) runs them without moving anything.

Each script runs through the CLI, once compiled (the default) and once
interpreted (`CLOISON_NO_JIT=1`), its body (what follows the `// expect:`
line, as the harness reads it) in a file of its own. A crash or another
result fails; one past the time limit is listed, not failed: what
collects by design takes hours like this, and a slower machine would make
others flaky.

Usage: scripts/evacuation-stress.py [--bin PATH] [--jobs N] [--limit S] [SCRIPT ...]
"""
import argparse, concurrent.futures, json, os, pathlib, subprocess, sys, tempfile, time

ROOT = pathlib.Path(__file__).resolve().parent.parent
MODES = {"compiled": {}, "interpreted": {"CLOISON_NO_JIT": "1"}}
WORDS = ("expect", "error", "compile error")


def parse(text):
    """The expectation (word, value) and the body, as tests/scripts.rs's
    `parse`: leading `//` lines, the first `// WORD: V` (or
    `// WORD-json: "V"`) ends them."""
    rest = text
    while rest.startswith("//"):
        line, _, after = rest.partition("\n")
        word, colon, arg = line[3:].partition(":") if line.startswith("// ") else ("", "", "")
        json_ = word.endswith("-json")
        word = word.removesuffix("-json")
        if colon and word in WORDS:
            arg = arg.removeprefix(" ")
            return word, json.loads(arg) if json_ else arg, after
        rest = after
    raise ValueError("no `// expect:`, `// error:` or `// compile error:` line")


def check(cli, path, mode, limit, tmp):
    word, want, body = parse(path.read_text())
    src = pathlib.Path(tmp) / f"{path.stem}.{mode}.js"
    src.write_text(body)
    env = {k: v for k, v in os.environ.items() if k != "CLOISON_NO_JIT"}
    env.update(CLOISON_GC_FULL="1", CLOISON_GC_EVACUATE="all", CLOISON_GC_STRESS="1",
               CLOISON_GC_NO_NATIVE_ROOTS="pin", **MODES[mode])
    start = time.monotonic()
    try:
        p = subprocess.run([cli, str(src)], env=env, capture_output=True, timeout=limit)
    except subprocess.TimeoutExpired:
        return "slow", ""
    secs = time.monotonic() - start
    out, err = p.stdout.decode(errors="replace"), p.stderr.decode(errors="replace")
    if p.returncode < 0 or p.returncode > 1:
        return "crash", f"exit {p.returncode}: {err.strip()[-300:]}"
    if word == "expect":
        # (`print`'s lines before the value.)
        ok = p.returncode == 0 and (out == want + "\n" or out.endswith("\n" + want + "\n"))
        got = out.strip()[-300:] or err.strip()[-300:]
    elif word == "error":
        ok = p.returncode == 1 and want in err
        got = err.strip()[-300:] or out.strip()[-300:]
    else:
        # (`cloison: PATH:LINE:COL: message`.)
        ok = p.returncode == 1 and err.rstrip("\n").endswith(f":{want}")
        got = err.strip()[-300:]
    return ("pass", f"{secs:.1f} s") if ok else ("wrong", f"want {word} {want!r}, got {got!r}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bin", default=str(ROOT / "target/release/cloison"))
    ap.add_argument("--jobs", type=int, default=os.cpu_count())
    ap.add_argument("--limit", type=float, default=120, help="seconds a run may take (default 120)")
    ap.add_argument("scripts", nargs="*", type=pathlib.Path)
    a = ap.parse_args()
    scripts = a.scripts or sorted((ROOT / "tests/scripts").glob("*.js"))
    runs = [(s, m) for s in scripts for m in MODES]
    bad, slow = [], []
    with tempfile.TemporaryDirectory() as tmp, concurrent.futures.ThreadPoolExecutor(a.jobs) as ex:
        futures = {ex.submit(check, a.bin, s, m, a.limit, tmp): (s, m) for s, m in runs}
        for f in concurrent.futures.as_completed(futures):
            s, m = futures[f]
            state, what = f.result()
            if state in ("crash", "wrong"):
                bad.append(f"{s.name} ({m}): {state}: {what}")
                print(bad[-1], flush=True)
            elif state == "slow":
                slow.append(f"{s.name} ({m})")
    print(f"{len(runs) - len(bad) - len(slow)} of {len(runs)} runs pass, {len(bad)} fail, "
          f"{len(slow)} past {a.limit:g} s")
    for s in sorted(slow):
        print(f"  slow: {s}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
