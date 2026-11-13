// The RegExp methods read neither themselves nor the other `Symbol`
// methods (ES2025 22.2.6): a wrapper of one calling the builtin, or the
// builtin called on a RegExp with a method of its own, gets the fast
// path (not the String method's, which would find that method again).
// And the species' quick check sees a custom species installed after
// `RegExp.prototype`'s shape changed.
// expect: 1,2 1,3 a#b# 1 a|b 5 aXc a|c 2 2 0 2 2 2

var out = [], calls = 0;
// Each method wrapped, delegating to the builtin.
for (const k of ["match", "matchAll", "replace", "search", "split"]) {
  const orig = RegExp.prototype[Symbol[k]];
  RegExp.prototype[Symbol[k]] = function (...a) { calls++; return orig.apply(this, a); };
}
out.push("a1b2".match(/\d/g).join(), [..."a1b2".matchAll(/\d/g)].map(m => m.index).join(), "a1b2".replace(/\d/g, "#"), "a1b2".search(/\d/), "a,b".split(/,/).join("|"), calls);
// A direct call of the builtin on a RegExp whose own method differs:
// the builtin's algorithm, which doesn't read it.
var re = /b/g; re[Symbol.replace] = () => "own"; re[Symbol.split] = () => "own";
out.push(RegExp.prototype[Symbol.replace].call(re, "abc", "X"));
var re2 = /b/; re2[Symbol.split] = () => "own";
out.push(RegExp.prototype[Symbol.split].call(re2, "abc").join("|"));
// The prototype's shape changed (an unrelated property), then a species.
var n = 0;
class S extends RegExp { constructor(...a) { n++; super(...a); } }
out.push("a,b".split(/,/).length);
RegExp.prototype.unrelated = 1;
out.push("a,b".split(/,/).length, n);
RegExp.prototype.constructor = { [Symbol.species]: S };
out.push("a,b".split(/,/).length, [..."aa".matchAll(/a/g)].length, n);
out.join(" ");
