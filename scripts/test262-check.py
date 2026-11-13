"""Compare a test262 run with the failures known.

    python3 -I scripts/test262-check.py [--update] KNOWN fails-all.txt

KNOWN (tests/test262-known-fails.txt) lists the tests that fail, one path
per line under test262's test/, after a `# test262 COMMIT` line naming
the checkout they were run at; fails-all.txt is what
scripts/test262-editions.py writes. Prints the tests that fail and
aren't known (with their message), and the known ones that pass now;
exits 1 if any fails that isn't known. `--update` rewrites KNOWN with
this run's failures instead.
"""
import sys

args = [a for a in sys.argv[1:] if a != '--update']
known_path, fails_path = args
lines = open(known_path).read().splitlines()
header = [l for l in lines if l.startswith('#')]
known = {l for l in lines if l and not l.startswith('#')}
fails = {}
for l in open(fails_path):
    path, _edition, msg = (l.rstrip('\n').split('\t') + ['', ''])[:3]
    fails[path] = msg

if '--update' in sys.argv:
    with open(known_path, 'w') as f:
        f.write('\n'.join(header + sorted(fails)) + '\n')
    print(f'{known_path}: {len(fails)} failures (was {len(known)})')
    sys.exit(0)

new = sorted(set(fails) - known)
fixed = sorted(known - set(fails))
for p in new:
    print(f'NEW FAILURE {p}\t{fails[p]}')
for p in fixed:
    print(f'now passes  {p}')
print(f'{len(fails)} failures: {len(new)} new, {len(fixed)} known that now pass'
      + (f' ({sum(fails[p] == "TIMEOUT" for p in new)} of the new are timeouts)' if new else ''))
if fixed:
    print(f'(scripts/test262-check.py --update {known_path} fails-all.txt removes those)')
sys.exit(1 if new else 0)
