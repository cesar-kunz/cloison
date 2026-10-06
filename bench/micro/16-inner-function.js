// Creating an inner function per call, used as a constructor with a borrowed prototype.
// features: loop, call, object, new, proto, closure
// expect: 600000
function Parent() {}
Parent.prototype.k = 3;
function derive(parent) {
  function Tmp() {}
  Tmp.prototype = parent.prototype;
  return new Tmp();
}
function main() {
  var sum = 0;
  for (var i = 0; i < 200000; i++) sum += derive(Parent).k;
  return sum;
}
main();
