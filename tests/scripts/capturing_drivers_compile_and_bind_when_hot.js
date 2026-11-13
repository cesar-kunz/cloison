// A small loop-free function whose registers its closures capture,
// recursing through them (EarleyBoyer's `deriv_trees`): it counts down
// to compiling, and a compiled caller's site binds once it has
// (`CallSite::wait_hot`). And `typeof` of each kind of value (its
// strings made once).
// expect: 534000:number,number,undefined,boolean,string,function,object,object,function

function Pair(car, cdr) { this.car = car; this.cdr = cdr; }
var trees;
function make(depth) {
  trees = function (n, k) {
    var loop;
    if (n <= 0) return new Pair(k, null);
    loop = function (i, acc) {
      while (i > 0) {
        var t = trees(n - 1, k + i);
        acc = new Pair(t.car + acc.car, acc);
        i--;
      }
      return acc;
    };
    return loop(2, new Pair(n, null));
  };
}
function kinds(v) { return typeof v; }
function main() {
  make();
  var t = 0;
  for (var r = 0; r < 3000; r++) t += trees(4, r & 7).car;
  var ks = [1, 1.5, undefined, true, "s", main, {}, null, Math.max].map(kinds).join();
  return t + ":" + ks;
}
main();
