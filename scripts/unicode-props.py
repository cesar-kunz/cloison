"""src/unicode_props.rs: the code points of the Unicode properties regexp
property escapes (`\\p{…}`) name, from the Unicode Character Database.

The properties are ECMA-262's (table-binary-unicode-properties, by the
names and aliases it lists, and the values of General_Category, Script
and Script_Extensions, table-nonbinary-unicode-properties, by the names
and aliases PropertyValueAliases.txt lists): a binary property's code
points from the file that defines it (Any, ASCII and Assigned are
ECMA-262's own), a general category's from DerivedGeneralCategory.txt
(the groups, L, LC, C and the others, are the union of theirs), a
script's from Scripts.txt (Unknown where none is listed) and its script
extension's from ScriptExtensions.txt (the code point's script where
none is listed). Katakana_Or_Hiragana (Hrkt) has no code points and
isn't written out, as test262 doesn't test it and V8, SpiderMonkey and
JavaScriptCore reject it.

A table of sets (identical ones once, in the order of test262's tests of
them, built-ins/RegExp/property-escapes/generated) and of the spellings
of their escapes (`Script=Latin`, `sc=Latn`, `L`, …), sorted, for
`regexp::Parser::property`'s binary search.

    curl -O https://www.unicode.org/Public/18.0.0/ucd/PropertyValueAliases.txt
    curl -O https://www.unicode.org/Public/18.0.0/ucd/PropList.txt
    curl -O https://www.unicode.org/Public/18.0.0/ucd/DerivedCoreProperties.txt
    curl -O https://www.unicode.org/Public/18.0.0/ucd/DerivedNormalizationProps.txt
    curl -O https://www.unicode.org/Public/18.0.0/ucd/Scripts.txt
    curl -O https://www.unicode.org/Public/18.0.0/ucd/ScriptExtensions.txt
    curl -O https://www.unicode.org/Public/18.0.0/ucd/extracted/DerivedGeneralCategory.txt
    curl -O https://www.unicode.org/Public/18.0.0/ucd/extracted/DerivedBinaryProperties.txt
    curl -O https://www.unicode.org/Public/18.0.0/ucd/emoji/emoji-data.txt
    python3 -I scripts/unicode-props.py <the directory they're in>
"""
import os, re, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UCD = sys.argv[1]
MAX = 0x10FFFF

# ECMA-262's table-binary-unicode-properties: each name and its alias.
BINARY = [
    ('ASCII',), ('ASCII_Hex_Digit', 'AHex'), ('Alphabetic', 'Alpha'), ('Any',),
    ('Assigned',), ('Bidi_Control', 'Bidi_C'), ('Bidi_Mirrored', 'Bidi_M'),
    ('Case_Ignorable', 'CI'), ('Cased',), ('Changes_When_Casefolded', 'CWCF'),
    ('Changes_When_Casemapped', 'CWCM'), ('Changes_When_Lowercased', 'CWL'),
    ('Changes_When_NFKC_Casefolded', 'CWKCF'), ('Changes_When_Titlecased', 'CWT'),
    ('Changes_When_Uppercased', 'CWU'), ('Dash',), ('Default_Ignorable_Code_Point', 'DI'),
    ('Deprecated', 'Dep'), ('Diacritic', 'Dia'), ('Emoji',), ('Emoji_Component', 'EComp'),
    ('Emoji_Modifier', 'EMod'), ('Emoji_Modifier_Base', 'EBase'),
    ('Emoji_Presentation', 'EPres'), ('Extended_Pictographic', 'ExtPict'),
    ('Extender', 'Ext'), ('Grapheme_Base', 'Gr_Base'), ('Grapheme_Extend', 'Gr_Ext'),
    ('Hex_Digit', 'Hex'), ('IDS_Binary_Operator', 'IDSB'), ('IDS_Trinary_Operator', 'IDST'),
    ('ID_Continue', 'IDC'), ('ID_Start', 'IDS'), ('Ideographic', 'Ideo'),
    ('Join_Control', 'Join_C'), ('Logical_Order_Exception', 'LOE'), ('Lowercase', 'Lower'),
    ('Math',), ('Noncharacter_Code_Point', 'NChar'), ('Pattern_Syntax', 'Pat_Syn'),
    ('Pattern_White_Space', 'Pat_WS'), ('Quotation_Mark', 'QMark'), ('Radical',),
    ('Regional_Indicator', 'RI'), ('Sentence_Terminal', 'STerm'), ('Soft_Dotted', 'SD'),
    ('Terminal_Punctuation', 'Term'), ('Unified_Ideograph', 'UIdeo'), ('Uppercase', 'Upper'),
    ('Variation_Selector', 'VS'), ('White_Space', 'space'), ('XID_Continue', 'XIDC'),
    ('XID_Start', 'XIDS'),
]


def records(name):
    """The data lines of a UCD file: (first, last, fields after the range)."""
    for line in open(os.path.join(UCD, name), encoding='utf-8'):
        line = line.split('#')[0].strip()
        if not line:
            continue
        f = [x.strip() for x in line.split(';')]
        a, _, b = f[0].partition('..')
        yield int(a, 16), int(b or a, 16), f[1:]


def merge(rs):
    out = []
    for a, b in sorted(rs):
        if out and a <= out[-1][1] + 1:
            out[-1] = (out[-1][0], max(b, out[-1][1]))
        else:
            out.append((a, b))
    return out


version = re.search(r'DerivedCoreProperties-([\d.]+)\.txt',
                    open(os.path.join(UCD, 'DerivedCoreProperties.txt')).readline()).group(1)

# The binary properties, from the files that list them (their lines of
# one property name; the files' other properties have a value too).
binary = {'Any': [(0, MAX)], 'ASCII': [(0, 0x7F)]}
for name in ('PropList.txt', 'DerivedCoreProperties.txt', 'DerivedNormalizationProps.txt',
             'DerivedBinaryProperties.txt', 'emoji-data.txt'):
    for a, b, f in records(name):
        if len(f) == 1:
            binary.setdefault(f[0], []).append((a, b))

# Property values and their aliases: (short, long, others…) a property.
values = {'gc': [], 'sc': []}
for line in open(os.path.join(UCD, 'PropertyValueAliases.txt'), encoding='utf-8'):
    f = [x.strip() for x in line.split('#')[0].split(';')]
    if f[0] in values:
        values[f[0]].append(f[1:])

gc = {}
for a, b, f in records('DerivedGeneralCategory.txt'):
    gc.setdefault(f[0], []).append((a, b))
binary['Assigned'] = [r for v, rs in gc.items() if v != 'Cn' for r in rs]
# A group's members, from its comment in PropertyValueAliases.txt (`L` is
# `Ll | Lm | Lo | Lt | Lu`).
for line in open(os.path.join(UCD, 'PropertyValueAliases.txt'), encoding='utf-8'):
    m = re.match(r'gc ; (\w+) .*# ([\w |]+)$', line.strip())
    if m:
        gc[m.group(1)] = [r for v in m.group(2).split(' | ') for r in gc[v]]

long_sc = {v[1]: v[0] for v in values['sc']}
sc = {}
for a, b, f in records('Scripts.txt'):
    sc.setdefault(long_sc[f[0]], []).append((a, b))
script = [None] * (MAX + 1)
for v, rs in sc.items():
    for a, b in rs:
        script[a:b + 1] = [v] * (b - a + 1)
sc['Zzzz'] = [(c, c) for c in range(MAX + 1) if script[c] is None]
exts = {}
for a, b, f in records('ScriptExtensions.txt'):
    for c in range(a, b + 1):
        exts[c] = f[0].split()
scx = {}
for c in range(MAX + 1):
    for v in exts.get(c, [script[c] or 'Zzzz']):
        scx.setdefault(v, []).append((c, c))

# Each property: test262's test's file name (the order of the sets), its
# spellings and its code points.
props = []
for names in BINARY:
    props.append((names[0], list(names), binary[names[0]]))
for v in values['gc']:
    props.append(('General_Category_-_' + v[1], [p + '=' + n for p in ('General_Category', 'gc') for n in v] + v, gc[v[0]]))
for key, prop, table in (('Script', ('Script', 'sc'), sc), ('Script_Extensions', ('Script_Extensions', 'scx'), scx)):
    for v in values['sc']:
        if v[0] in table:
            props.append((key + '_-_' + v[1], [p + '=' + n for p in prop for n in v], table[v[0]]))
# (Ahom, Thai, … are their scripts' short and long names both.)
props = [(list(dict.fromkeys(names)), merge(rs)) for _, names, rs in sorted(props, key=lambda p: p[0] + '.js')]

sets, order = {}, []
for _, r in props:
    if tuple(r) not in sets:
        sets[tuple(r)] = len(order)
        order.append(r)
names = sorted((n, sets[tuple(r)]) for ns, r in props for n in ns)
assert len(set(n for n, _ in names)) == len(names)

out = [
    "// Generated by scripts/unicode-props.py from the Unicode Character",
    "// Database (%s): don't edit." % version,
    "",
    "/// The properties' code points: sorted inclusive ranges.",
    "pub(crate) static SETS: [&[(u32, u32)]; %d] = [" % len(order),
]
out += ['    &[' + ', '.join('(0x%X, 0x%X)' % ab for ab in r) + '],' for r in order]
out += [
    "];",
    "",
    "/// Each property escape's spelling (`Script=Latin`, `sc=Latn`, `L`, …)",
    "/// and its set in [`SETS`], sorted by spelling.",
    "pub(crate) static NAMES: [(&str, u16); %d] = [" % len(names),
]
out += ['    ("%s", %d),' % (n, i) for n, i in names]
out.append("];")
open(os.path.join(ROOT, 'src/unicode_props.rs'), 'w').write('\n'.join(out) + '\n')
print(len(props), 'properties,', len(order), 'sets,', len(names), 'names')
