// Methods reading a global variable that holds an object, and calling through it.
// features: loop, call, object, new, proto, global
// expect: 4000000
function P() { this.n = 0; }
P.prototype.note = function (c) { this.n += c.k; return true; };
var service = new P();
function C(k) { this.k = k; }
C.prototype.go = function () { return service.note(this); };
function main() {
  var c = new C(3), m = 0;
  for (var i = 0; i < 1000000; i++) if (c.go()) m++;
  return service.n + m;
}
main();
