#!/usr/bin/env python3
"""Count the lines with `unsafe` in each source file, against a baseline.

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


def counts():
    out = {}
    for p in sorted((root / "src").rglob("*.rs")):
        n = sum(1 for line in p.read_text().splitlines() if word.search(line.split("//", 1)[0]))
        if n:
            out[str(p.relative_to(root))] = n
    return out


def main():
    now = counts()
    if sys.argv[1:] == ["--update"]:
        baseline.write_text("".join(f"{n} {f}\n" for f, n in now.items()))
        print(f"{sum(now.values())} lines with unsafe in {len(now)} files")
        return 0
    old = {}
    for line in baseline.read_text().splitlines():
        n, f = line.split(" ", 1)
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
