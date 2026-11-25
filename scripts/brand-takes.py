#!/usr/bin/env python3
"""Check that every `Fresh::take()` bound to a name has its reason.

A branded call's result (`Fresh`) can't be held across another call
that may collect: the borrow checker sees to it. `take()` gives the raw
value, which it no longer can check, so a binding of one must say why
it is safe, on the line above: `// take: <reason>`. A value used at
once (an argument, a test) needs none.

Usage: scripts/brand-takes.py [--list]
"""
import pathlib, re, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
# A binding, of one name or a pattern, whose value is a taken Fresh.
BIND = re.compile(r'^\s*let\b.*(\?\.take\(\)|Fresh::take)')
# ... but not one that is a number or a boolean made of it (no cell), one
# rooted at once, or a closure's definition.
EXEMPT = re.compile(
    r'to_boolean\(|to_number\(|\.as_number\(\)|\.as_i32|\.to_number\(\)|\.is_\w+\(\)'
    r'|\bsc\.root\(|\.root\(&|=\s*\|vm\b|\|vm: &mut Vm'
)


def main():
    bad = []
    for path in sorted((ROOT / 'src').rglob('*.rs')):
        if path.name == 'brand.rs':
            continue
        lines = path.read_text().split('\n')
        for i, line in enumerate(lines):
            if not BIND.match(line) or EXEMPT.search(line):
                continue
            j = i - 1
            while j >= 0 and lines[j].strip().startswith('//') and '// take:' not in lines[j]:
                j -= 1
            reason = j >= 0 and '// take:' in lines[j]
            if not reason or 'TODO' in lines[j]:
                bad.append(f'{path.relative_to(ROOT)}:{i + 1}: {line.strip()}')
    for b in bad:
        print(b)
    if bad:
        print(f'{len(bad)} taken bindings without a `// take:` reason', file=sys.stderr)
        sys.exit(1)


main()
