// `matchAll` and `split` copy the RegExp by its SpeciesConstructor, a
// construction whose IsRegExp reads its `Symbol.match` (ES2025
// 22.2.6.9 steps 4–6, 22.2.6.14 steps 4–10): their fast paths are only
// for a builtin `constructor`, `Symbol.species` and `Symbol.match`.
// expect: 2 5 b,b 2 4 b,b 2 6 b,b 2 6

var n = 0, reads = 0, out = [];
class S extends RegExp { constructor(...a) { n++; super(...a); } }
var re = /a/g; re.constructor = { [Symbol.species]: S };
[..."aa".matchAll(re)];
var re2 = /a/; re2.constructor = { [Symbol.species]: S };
"bab".split(re2);
function counted(r) { Object.defineProperty(r, Symbol.match, { get() { reads++; return RegExp.prototype[Symbol.match]; } }); return r; }
[..."aa".matchAll(counted(/a/g))];
"bab".split(counted(/a/));
RegExp.prototype[Symbol.matchAll].call(counted(/a/g), "aa");
RegExp.prototype[Symbol.split].call(counted(/a/), "bab");
out.push(n, reads);
RegExp.prototype.constructor = { [Symbol.species]: S };
out.push("bab".split(/a/).join(), [..."aa".matchAll(/a/g)].length, n);
RegExp.prototype.constructor = RegExp;
var d = Object.getOwnPropertyDescriptor(RegExp, Symbol.species);
Object.defineProperty(RegExp, Symbol.species, { get() { return S; }, configurable: true });
out.push("bab".split(/a/).join(), [..."aa".matchAll(/a/g)].length, n);
Object.defineProperty(RegExp, Symbol.species, d);
out.push("bab".split(/a/).join(), [..."aa".matchAll(/a/g)].length, n);
out.join(" ");
