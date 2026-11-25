// Binding a proxy: the prototype its `getPrototypeOf` trap gives, held
// by nothing else, outlives the `name` read's `get` trap, which may
// collect.
// expect: 0

var bad = 0;
for (var i = 0; i < 30; i++) {
  var p = new Proxy(function () {}, {
    getPrototypeOf() { var o = { mark: i }; o.self = o; return o; },
    get(t, k) {
      var junk = [];
      for (var j = 0; j < 300; j++) junk.push({ j: j, s: 'x' + j });
      return Reflect.get(t, k);
    },
  });
  var pr = Object.getPrototypeOf(Function.prototype.bind.call(p));
  if (!pr || pr.mark !== i || pr.self !== pr) bad++;
}
bad;
