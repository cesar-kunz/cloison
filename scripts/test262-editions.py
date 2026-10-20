"""test262 pass rates by ECMAScript edition.

Every test under test/{built-ins,language,annexB} (not intl402 or
staging), run as test262's INTERPRETING.md says: strict and sloppy for a
test without `onlyStrict`/`noStrict`/`raw` (both must pass), `negative`
tests must fail in their phase with their error, `async` ones must print
`Test262:AsyncTestComplete`, `CanBlockIsFalse` ones run with `--no-block`.
A module test runs as a module (`--module`),
the harness a script before it (`--prelude`); every test's file is
written next to it, where its fixtures are.

A test's edition is the latest of its features' (ES5 for one with none:
test262 tags features from ES2015 on, mostly); a feature not in a
published edition counts as "proposal". Harness-only features don't
count.

    python3 -I scripts/test262-editions.py [path-filter]

Writes editions.tsv (edition, passed, total) and fails-all.txt in the
current directory.
"""
import os, re, sys, subprocess, collections, uuid
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
T = f'{ROOT}/test262'
CLOISON = f'{ROOT}/target/release/cloison'
only = sys.argv[1] if len(sys.argv) > 1 else ''

ED = {
    2015: '''ArrayBuffer DataView DataView.prototype.getFloat32 DataView.prototype.getFloat64
        DataView.prototype.getInt16 DataView.prototype.getInt32 DataView.prototype.getInt8
        DataView.prototype.getUint16 DataView.prototype.getUint32 DataView.prototype.setUint8
        Float32Array Float64Array Int8Array Int16Array Int32Array Uint8Array Uint16Array
        Uint32Array Uint8ClampedArray TypedArray Map Set WeakMap WeakSet Promise Proxy
        proxy-missing-checks Reflect Reflect.construct Reflect.set Reflect.setPrototypeOf
        Symbol Symbol.hasInstance Symbol.isConcatSpreadable Symbol.iterator Symbol.match
        Symbol.replace Symbol.search Symbol.species Symbol.split Symbol.toPrimitive
        Symbol.toStringTag Symbol.unscopables arrow-function class computed-property-names
        const let default-parameters destructuring-assignment destructuring-binding for-of
        generators new.target rest-parameters super template String.fromCodePoint
        String.prototype.endsWith String.prototype.includes Object.is Array.prototype.values
        tail-call-optimization __proto__''',
    2016: 'Array.prototype.includes exponentiation',
    2017: 'async-functions Atomics SharedArrayBuffer __getter__ __setter__',
    2018: '''async-iteration object-rest object-spread Promise.prototype.finally regexp-dotall
        regexp-lookbehind regexp-named-groups regexp-unicode-property-escapes
        Symbol.asyncIterator''',
    2019: '''Array.prototype.flat Array.prototype.flatMap Object.fromEntries
        optional-catch-binding string-trimming String.prototype.trimEnd
        String.prototype.trimStart Symbol.prototype.description json-superset
        well-formed-json-stringify stable-array-sort u180e''',
    2020: '''BigInt Promise.allSettled String.prototype.matchAll Symbol.matchAll
        coalesce-expression optional-chaining dynamic-import globalThis import.meta
        export-star-as-namespace-from-module for-in-order''',
    2021: '''AggregateError Promise.any String.prototype.replaceAll logical-assignment-operators
        numeric-separator-literal WeakRef FinalizationRegistry
        align-detached-buffer-semantics-with-web-reality''',
    2022: '''class-fields-private class-fields-public class-methods-private
        class-static-fields-private class-static-fields-public class-static-methods-private
        class-static-block class-fields-private-in Array.prototype.at String.prototype.at
        TypedArray.prototype.at Object.hasOwn error-cause regexp-match-indices
        top-level-await arbitrary-module-namespace-names''',
    2023: 'array-find-from-last change-array-by-copy hashbang symbols-as-weakmap-keys stable-typedarray-sort',
    2024: '''Atomics.waitAsync array-grouping arraybuffer-transfer promise-with-resolvers
        regexp-v-flag resizable-arraybuffer String.prototype.isWellFormed
        String.prototype.toWellFormed''',
    2025: '''import-attributes json-modules iterator-helpers set-methods
        regexp-duplicate-named-groups regexp-modifiers Float16Array promise-try RegExp.escape''',
    2026: '''Error.isError Math.sumPrecise Array.fromAsync uint8array-base64 upsert
        iterator-sequencing json-parse-with-source Atomics.pause explicit-resource-management''',
}
EDITION = {f: y for y, fs in ED.items() for f in fs.split()}
HARNESS = {'cross-realm', 'caller', 'IsHTMLDDA', 'host-gc-required', 'canonical-tz'}

def meta(src):
    m = re.search(r'/\*---(.*?)---\*/', src, re.S)
    y = m.group(1) if m else ''
    def lst(k):
        m = re.search(k + r':\s*\[(.*?)\]', y, re.S)
        if m:
            return [x.strip() for x in m.group(1).split(',') if x.strip()]
        m = re.search(k + r':\s*\n((?:\s+-.*\n?)+)', y)
        return [x.strip()[1:].strip() for x in m.group(1).splitlines()] if m else []
    neg = re.search(r'negative:\s*\n\s*phase:\s*(\w+)\s*\n\s*type:\s*(\w+)', y)
    return lst('features'), lst('flags'), lst('includes'), (neg.groups() if neg else None)

def edition(features):
    ys = [EDITION.get(f, 'proposal') for f in features if f not in HARNESS]
    if 'proposal' in ys:
        return 'proposal'
    return max(ys, default=5)

H = {}
def harness(n):
    if n not in H:
        p = f'{T}/harness/{n}'
        H[n] = open(p).read() if os.path.exists(p) else ''
    return H[n]

def run_once(src, strict, neg, is_async, test_dir, module=None, flags=()):
    n = os.path.join(test_dir, f'__t262_{uuid.uuid4().hex}.js')
    with open(n, 'w') as f:
        f.write(('"use strict";\n' if strict else '') + src)
    try:
        nb = ['--no-block'] if 'CanBlockIsFalse' in flags else []
        cmd = [CLOISON] + nb + (['--prelude', n, '--module', module] if module else [n])
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
        out = (r.stderr or '') + (r.stdout or '')
        if r.returncode < 0:
            return False, 'CRASH'
        if neg:
            phase, typ = neg
            if r.returncode == 0:
                return False, 'no error'
            if 'unsupported' in out:
                return False, out.strip()[-100:]
            if phase == 'runtime':
                return f'uncaught {typ}' in out, out.strip()[-100:]
            # (An early error: cloison reports it without "uncaught".)
            return 'uncaught' not in out, out.strip()[-100:]
        if r.returncode != 0:
            return False, out.strip()[-100:]
        if is_async and 'Test262:AsyncTestComplete' not in out:
            return False, out.strip()[-100:] or 'async test incomplete'
        return True, ''
    except subprocess.TimeoutExpired:
        return False, 'TIMEOUT'
    finally:
        os.unlink(n)

def run(p):
    s = open(p, encoding='utf8', errors='replace').read()
    ft, fl, inc, neg = meta(s)
    ed = edition(ft)
    is_async = 'async' in fl
    if 'raw' in fl:
        pre = ''
    else:
        names = ['assert.js', 'sta.js'] + (['doneprintHandle.js'] if is_async else []) + inc
        pre = ''.join(harness(i) for i in names) + '\n'
    modes = [True] if 'onlyStrict' in fl else [False] if ('noStrict' in fl or 'raw' in fl) else [False, True]
    if 'module' in fl:
        ok, msg = run_once(pre, False, neg, is_async, os.path.dirname(p), p, fl)
        return p, ed, ok, msg.replace('\n', ' ')
    for strict in modes:
        ok, msg = run_once(pre + s, strict, neg, is_async, os.path.dirname(p), None, fl)
        if not ok:
            return p, ed, False, ('strict: ' if strict else '') + msg.replace('\n', ' ')
    return p, ed, True, ''

files = []
for d in ['built-ins', 'language', 'annexB']:
    for r, _, fs in os.walk(f'{T}/test/{d}'):
        for f in fs:
            p = f'{r}/{f}'
            if f.endswith('.js') and '_FIXTURE' not in f and not f.startswith('__t262_') and only in p:
                files.append(p)
with ThreadPoolExecutor(os.cpu_count()) as ex:
    res = list(ex.map(run, sorted(files)))
tally = collections.defaultdict(lambda: [0, 0])
for _, ed, ok, _ in res:
    tally[ed][0] += ok
    tally[ed][1] += 1
order = sorted((k for k in tally if k != 'proposal'), key=int) + (['proposal'] if 'proposal' in tally else [])
with open('editions.tsv', 'w') as f:
    for k in order:
        a, b = tally[k]
        name = 'ES5' if k == 5 else f'ES{k}' if k != 'proposal' else 'proposal'
        f.write(f'{name}\t{a}\t{b}\n')
        print(f'{name:10} {a:6} / {b:6}  {100 * a / b:5.1f}%')
a = sum(ok for _, _, ok, _ in res)
print(f'{"all":10} {a:6} / {len(res):6}  {100 * a / len(res):5.1f}%')
with open('fails-all.txt', 'w') as f:
    for p, ed, ok, m in res:
        if not ok:
            f.write(f'{p[len(T) + 6:]}\t{ed}\t{m}\n')
