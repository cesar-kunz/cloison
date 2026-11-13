// A load into the slot of the register it loads from (`o = o.o`, as
// `get_own_nc_g1_to_g1`): on arm64 a family template, whose registers the
// fused name must give.
// expect: 2450000

function P(x) { this.x = x; this.o = { k: x }; }
function f(o) { return o.x + o.o.k; }
function main() {
  var objs = [0];
  for (var i = 0; i < 50; i++) objs.push(new P(i));
  var t = 0;
  for (var r = 0; r < 1000; r++) for (var i = 1; i < objs.length; i++) t += f(objs[i]);
  return t;
}
main();
