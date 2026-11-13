// Stores to a function's fields given up to the runtime (`set_named_rt`):
// a counter never set (`NaN`, stored again and again, its field constant),
// a constructor's `prototype` replaced.
// expect: 1999464,NaN,3000,7

var B = {};
function W() {}
B.W = W;
W.Distance = function (x) { ++W.calls; var t = 0; for (var i = 0; i < 20; i++) t += x * i; ++W.iters; W.last = x; return t; };
W.iters = 0;
function P() {}
var protos = [{ k: 1 }, { k: 2 }];
function make(r) { P.prototype = protos[r >> 9 & 1]; return new P().k; }
function main() {
  var t = 0;
  for (var r = 0; r < 3000; r++) t += W.Distance(r & 7) + make(r);
  return [t, W.calls, W.iters, W.last].join();
}
main();
