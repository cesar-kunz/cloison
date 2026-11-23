// `then`'s SpeciesConstructor reads `@@species` from the `constructor` a
// getter returns, which only it refers to: a proxy whose handler's `get`
// getter collects is still the receiver its trap gets.
// expect: object50,object50,object50
function churn() { $262.gc(); var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i }); return junk.length; }
var out = [];
for (var round = 0; round < 3; round++) {
  var p = Promise.resolve(round);
  Object.defineProperty(p, "constructor", { get() {
    var t = {};
    for (var i = 0; i < 50; i++) t["p" + i] = { x: i };
    return new Proxy(t, { get get() { churn(); return function (t, key, r) { out.push(typeof r + Object.keys(r).length); return undefined; }; } });
  } });
  p.then(function () {});
}
out.join(",");
