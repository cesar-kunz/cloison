// A getter and a setter on a prototype (Object.defineProperty), used in a hot loop.
// features: loop, object, new, proto, define-property, getter, call
// expect: 131500000
function Temp(c) { this._c = c; }
Object.defineProperty(Temp.prototype, "fahrenheit", {
  get: function () { return this._c * 9 / 5 + 32; },
  set: function (f) { this._c = (f - 32) * 5 / 9; }
});
function main() {
  var t = new Temp(0), s = 0;
  for (var i = 0; i < 1000000; i++) {
    t.fahrenheit = (i % 200) + 32;
    s += t.fahrenheit;
  }
  return Math.round(s);
}
main();
