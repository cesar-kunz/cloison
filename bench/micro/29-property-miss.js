// Loading absent properties: the lookup walks to Object.prototype and yields undefined.
// features: loop, object, new, proto, define-property
// expect: 1000000
Object.defineProperty(Object.prototype, "extra", { value: 1 });
function P() { this.a = 1; }
function main() {
  var p = new P(), n = 0;
  for (var i = 0; i < 500000; i++) {
    if (p.missing === undefined) n++;
    n += p.extra;
  }
  return n;
}
main();
