#!/usr/bin/env python3
"""Check that every raw value bound to a name out of a branded result
(`Fresh::take()`, or a `Gc`'s `.v()`/`.obj()` from `vm.mu()`) has its
reason.

A branded call's result (`Fresh`) or a new cell (`Gc`) can't be held
across a call that may collect: the borrow checker sees to it. `take()`,
`.v()` and `.obj()` give the raw value, which it no longer can check, so
a binding of one must say why it is safe, on the line above:
`// take: <reason>`. A `Raw` value (`.raw()`, `.raw_obj()`), taken where
nothing collects until its last use, says so with `// raw: <reason>`:
the `gc-check` feature checks that claim at run time. A value used at
once (an argument, a test) needs none.

Usage: scripts/brand-takes.py [--list]
"""
import pathlib, re, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
# A binding, of one name or a pattern, whose value is a taken Fresh.
BIND = re.compile(r'^\s*let\b.*(\?\.take\(\)|Fresh::take|\.mu\(\)\.\w+\(.*\)\.(v|obj)\(\)|\.raw(_obj)?\(\))')
# ... but not one that is a number or a boolean made of it (no cell), one
# rooted at once, or a closure's definition.
EXEMPT = re.compile(
    r'to_boolean\(|to_number\(|\.as_number\(\)|\.as_i32|\.to_number\(\)|\.is_\w+\(\)'
    r'|\bsc\.root\(|\.roots?\(|\.root\(&|=\s*\|vm\b|\|vm: &mut Vm'
)


# (A `Raw` value's, `// raw:`: checked at run time too, see `brand::Raw`.)
REASON = re.compile(r'// (take|raw):')


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
            while j >= 0 and lines[j].strip().startswith('//') and not REASON.search(lines[j]):
                j -= 1
            reason = j >= 0 and REASON.search(lines[j])
            if not reason or 'TODO' in lines[j]:
                bad.append(f'{path.relative_to(ROOT)}:{i + 1}: {line.strip()}')
    for b in bad:
        print(b)
    if bad:
        print(f'{len(bad)} taken bindings without a `// take:` or `// raw:` reason', file=sys.stderr)
        sys.exit(1)


main()
