#!/usr/bin/env python3
"""Summarize a sweep.sh TSV: per setting, each workload's time and peak RSS
against the best any cloison setting reached on it (geometric mean and
worst), against the engines' best where they ran, and the settings no other
beats on both time and memory (the Pareto front).

    python3 bench/sparse/pareto.py OUT.tsv [--top N]
"""
import csv, math, sys
from collections import defaultdict

path = sys.argv[1]
top = int(sys.argv[sys.argv.index('--top') + 1]) if '--top' in sys.argv else 15
rows = list(csv.DictReader(open(path), delimiter='\t'))
engines = {'v8', 'sm', 'jsc'}
TIMEOUT = 120000

cell = {}
for r in rows:
    ms = float(r['ms']) if r['ms'] else TIMEOUT
    rss = float(r['rss_kb']) if r['rss_kb'] not in ('', '-1') else 64e6  # a timeout: 64 GB
    cell[(r['setting'], r['workload'] + ' ' + r['sizes'])] = (max(ms, 1.0), max(rss, 1.0))

settings = sorted({s for s, _ in cell})
cl = [s for s in settings if s not in engines]
works = sorted({w for _, w in cell})
best_t = {w: min(cell[(s, w)][0] for s in cl if (s, w) in cell) for w in works}
best_m = {w: min(cell[(s, w)][1] for s in cl if (s, w) in cell) for w in works}
eng_t = {w: min((cell[(e, w)][0] for e in engines if (e, w) in cell), default=None) for w in works}
eng_m = {w: min((cell[(e, w)][1] for e in engines if (e, w) in cell), default=None) for w in works}

def gmean(xs):
    return math.exp(sum(math.log(x) for x in xs) / len(xs)) if xs else float('nan')

score = {}
for s in cl:
    ws = [w for w in works if (s, w) in cell]
    if len(ws) < len(works):
        continue
    tr = [cell[(s, w)][0] / best_t[w] for w in ws]
    mr = [cell[(s, w)][1] / best_m[w] for w in ws]
    worst_t = max(zip(tr, ws))
    worst_m = max(zip(mr, ws))
    et = [cell[(s, w)][0] / eng_t[w] for w in ws if eng_t[w]]
    em = [cell[(s, w)][1] / eng_m[w] for w in ws if eng_m[w]]
    score[s] = (gmean(tr), gmean(mr), worst_t, worst_m, gmean(et), gmean(em))

front = [s for s in score if not any(
    score[o][0] <= score[s][0] and score[o][1] <= score[s][1] and score[o][:2] != score[s][:2] for o in score)]

print(f'{len(works)} workloads, {len(score)} settings complete; time and memory as multiples of the best setting per workload\n')
hdr = f"{'setting':22} {'time':>6} {'mem':>6}  {'worst time':>28}  {'worst mem':>28}  {'t/eng':>6} {'m/eng':>6}"
print(hdr)
for s in sorted(score, key=lambda s: score[s][0] * score[s][1])[:top]:
    gt, gm, (wt, wtw), (wm, wmw), et, em = score[s]
    mark = '*' if s in front else ' '
    print(f'{mark}{s:21} {gt:6.2f} {gm:6.2f}  {wt:6.1f} {wtw:21}  {wm:6.1f} {wmw:21}  {et:6.2f} {em:6.2f}')
print('\n* on the Pareto front (no setting both faster and smaller):', ', '.join(sorted(front, key=lambda s: score[s][0])))

print(f"\n{'workload':24}" + ''.join(f'{s[:14]:>16}' for s in sorted(front, key=lambda s: score[s][0])[:5]) + f"{'engines best':>16}")
for w in works:
    line = f'{w:24}'
    for s in sorted(front, key=lambda s: score[s][0])[:5]:
        t, m = cell[(s, w)]
        line += f'{t:8.0f}ms{m / 1024:5.0f}M'
    if eng_t[w]:
        line += f'{eng_t[w]:8.0f}ms{eng_m[w] / 1024:5.0f}M'
    print(line)
