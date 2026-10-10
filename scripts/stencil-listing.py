#!/usr/bin/env python3
"""List the stencils as the JIT emits them, from the generated table.

    scripts/stencil-listing.py [--target TRIPLE] [NAME-PREFIX ...]

Reads `stencils.rs` from the newest build directory (the host's, or the
given target's), after build.rs's rewrites (stub folding, branch inversion,
templates), and disassembles each stencil with capstone's `cstool`, marking
the holes and continuations at their relocations and, for an arm64
template, the slot-register fields. The instruction count excludes a
trailing `b toi_continue`, which the JIT trims when the next piece follows.
"""
import glob, os, re, subprocess, sys

args = sys.argv[1:]
target = ""
if args and args[0] == "--target":
    target = args[1]; args = args[2:]
root = os.path.join(os.path.dirname(__file__), "..", "target", target, "release", "build")
paths = glob.glob(os.path.join(root, "toi-*", "out", "stencils.rs")) + glob.glob(os.path.join(root, "toi", "*", "out", "stencils.rs"))
path = max(paths, key=os.path.getmtime)
arch = "aarch64" if ("aarch64" in target or (not target and os.uname().machine in ("arm64", "aarch64"))) else "x86_64"
src = open(path).read()

SYM = {"HoleImm": "imm", "HoleImm64": "imm64", "HoleImm64b": "imm64b", "HoleReg": "reg",
       "Continue": "continue", "JumpTarget": "target", "Exit": "exit", "Slow": "slow"}
entries = []
for m in re.finditer(r'Stencil \{ name: "([^"]*)", slow: (\w+), slow_at: \d+, code: &\[([^\]]*)\], relocs: &\[(.*?)\], template: (None|Some\(&Template \{ fields: &\[(.*?)\], covers)', src):
    name, code, relocs, fields = m.group(1), m.group(3), m.group(4), m.group(6)
    code = bytes(int(x) for x in code.split(",") if x.strip())
    rl = []
    for r in re.finditer(r'R\((\d+), (\d+), Sym::(\w+)(?:\((\d+)\))?, (-?\d+)\)', relocs):
        off, ty, sym, rt, add = r.groups()
        rl.append((int(off), int(ty), SYM.get(sym, sym) + (f"rt{rt}" if rt else ""), int(add)))
    fl = [tuple(int(v) for v in f.groups()) for f in re.finditer(r'\((\d+), (\d+), (\d+)\)', fields or "")]
    entries.append((name, code, rl, fl))

def disasm(code):
    if not code:
        return []
    out = subprocess.run(["cstool", "arm64" if arch == "aarch64" else "x64", code.hex()], capture_output=True, text=True).stdout
    insns = []
    for line in out.splitlines():
        mm = re.match(r'\s*([0-9a-f]+)\s+((?:[0-9a-f]{2} )+)\s*(.*)', line)
        if mm:
            insns.append((int(mm.group(1), 16), len(mm.group(2).split()), mm.group(3).strip()))
    return insns

for name, code, relocs, fields in entries:
    if args and not any(name.startswith(a) for a in args):
        continue
    insns = disasm(code)
    n = len(insns)
    last = insns[-1] if insns else None
    trimmed = last and any(r[2] == "continue" and r[0] == last[0] + (0 if arch == "aarch64" else 1) for r in relocs)
    print(f"== {name}: {n - (1 if trimmed else 0)} instructions{' (+ trimmed b continue)' if trimmed else ''}, {len(code)} bytes")
    for off, size, text in insns:
        notes = [r[2] + (f"{r[3]:+d}" if r[3] else "") for r in relocs if off <= r[0] < off + size]
        notes += [f"g{p}@{bit}" for (foff, bit, p) in fields if foff == off]
        print(f"  {off:4x}: {text:40} {'; ' + ', '.join(notes) if notes else ''}")
