#!/usr/bin/env python3
"""Count the lines with `unsafe` in each source file (src/, the stencils,
the build script), and the `Obj::from_ptr` bridges (where code not
converted to `Obj` yet hands a raw pointer over), against a baseline.

    python3 -I scripts/unsafe-count.py          # fail if a file's count isn't its baseline
    python3 -I scripts/unsafe-count.py --update # write the current counts as the baseline

The baseline (scripts/unsafe-baseline.txt) only goes down: a change that
adds an `unsafe` fails CI unless it justifies it and updates the
baseline; one that removes some updates it too, so the room it made
isn't taken by the next. (A line counts when `unsafe` is in its code,
not in a comment.)
"""
import pathlib
import re
import sys

root = pathlib.Path(__file__).resolve().parent.parent
baseline = root / "scripts" / "unsafe-baseline.txt"
word = re.compile(r"\bunsafe\b")
bridge = re.compile(r"\bObj::from_ptr\(")
BRIDGES = "(Obj::from_ptr bridges)"


def sources():
    for d in ("src", "stencils", "build"):
        yield from sorted((root / d).rglob("*.rs"))
    yield root / "build.rs"


def counts():
    out, bridges = {}, 0
    for p in sources():
        code = [line.split("//", 1)[0] for line in p.read_text().splitlines()]
        n = sum(1 for line in code if word.search(line))
        if n:
            out[str(p.relative_to(root))] = n
        bridges += sum(len(bridge.findall(line)) for line in code if "fn from_ptr" not in line)
    out[BRIDGES] = bridges
    return out


def main():
    now = counts()
    if sys.argv[1:] == ["--update"]:
        baseline.write_text("".join(f"{n} {f}\n" for f, n in now.items()))
        lines = sum(n for f, n in now.items() if f != BRIDGES)
        print(f"{lines} lines with unsafe in {len(now) - 1} files, {now[BRIDGES]} Obj::from_ptr bridges")
        return 0
    old = {}
    for line in baseline.read_text().splitlines():
        n, f = line.split(" ", 1)  # (a file, or the bridges' count)
        old[f] = int(n)
    status = 0
    for f in sorted(set(now) | set(old)):
        n, o = now.get(f, 0), old.get(f, 0)
        if n > o:
            print(f"{f}: {n} lines with unsafe, baseline {o}: remove the new ones, or justify them")
            status = 1
        elif n < o:
            print(f"{f}: {n} lines with unsafe, baseline {o}")
            status = 1
    if status:
        print("then run scripts/unsafe-count.py --update")
    return status


sys.exit(main())
