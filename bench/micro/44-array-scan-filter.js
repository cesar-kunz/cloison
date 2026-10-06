// Scanning an array of objects, calling a predicate method, collecting matches.
// features: loop, call, object, new, proto, array
// expect: 196020
function C(k, on) { this.k = k; this.on = on; }
C.prototype.ok = function () { return this.on; };
function main() {
  var cs = [];
  for (var i = 0; i < 100; i++) cs.push(new C(i, i % 3 != 0));
  var total = 0;
  for (var r = 0; r < 3000; r++) {
    var skip = cs[r % 100], out = [];
    for (var i = 0; i < cs.length; i++) {
      var c = cs[i];
      if (c != skip && c.ok()) out.push(c);
    }
    total += out.length;
  }
  return total;
}
main();
