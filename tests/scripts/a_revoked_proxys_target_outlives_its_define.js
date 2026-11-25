// A proxy without a `defineProperty` trap forwards the define to its
// target: revoked by the trap's getter, or by the define's own `length`
// conversion, the target, held by nothing else then, outlives that
// conversion, which may collect.
// expect: 0,0

function run(inGetter) {
  var bad = 0;
  for (var i = 0; i < 30; i++) {
    var revoke;
    var handler = {};
    Object.defineProperty(handler, 'defineProperty', {
      get() { if (inGetter) revoke(); return undefined; },
    });
    var r = Proxy.revocable([1, 2, 3, { mark: i }], handler);
    revoke = r.revoke;
    var len = {
      valueOf() {
        if (!inGetter) revoke();
        var junk = [];
        for (var j = 0; j < 300; j++) junk.push({ j: j, s: 'x' + j });
        return 2;
      },
    };
    try { Object.defineProperty(r.proxy, 'length', { value: len }); } catch (e) { bad++; }
  }
  return bad;
}
[run(true), run(false)].join();
