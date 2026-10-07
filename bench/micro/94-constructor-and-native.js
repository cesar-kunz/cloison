// A constructor and a native method (`push`) in the same loop.
// toi: direct `new`; native results typed by their declared return type
// features: loop, object, new, array
// expect: 40000000000
function Box(k) { this.v = k; }
function main() {
  var a = [], s = 0;
  for (var i = 0; i < 200000; i++) { s = s + new Box(i).v; s = s + a.push(i); }
  return s;
}
main();
