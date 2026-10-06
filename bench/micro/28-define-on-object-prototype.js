// A non-enumerable method installed on Object.prototype with Object.defineProperty, called on functions.
// features: loop, call, object, new, proto, closure, define-property, for-in
// expect: 1000002
Object.defineProperty(Object.prototype, "extend", {
  value: function (parent) {
    function Tmp() {}
    Tmp.prototype = parent.prototype;
    this.prototype = new Tmp();
    this.parent = parent;
  }
});
function Base() {}
Base.prototype.k = function () { return 1; };
function main() {
  var sum = 0;
  for (var i = 0; i < 100000; i++) {
    var Sub = function () {};
    Sub.extend(Base);
    sum += new Sub().k();
  }
  var keys = 0, o = {a: 1, b: 2};
  for (var p in o) keys++;          // "extend" must not be enumerated
  return sum * 10 + keys;
}
main();
