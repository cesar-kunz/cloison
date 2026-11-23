// A ShadowRealm's wrapped function is held across its target's `name`
// getter, run by CopyNameAndLength: the collections the getter causes
// keep it, which only the wrapping refers to, alive.
// expect: nm 0 7
var r = new ShadowRealm();
var f = r.evaluate(`
  var f = function () { return 7; };
  Object.defineProperty(f, 'name', { get() { var a = []; for (var i = 0; i < 2000; i++) a.push({ i: i, s: 'x' + i }); return 'nm'; } });
  f`);
var g = []; for (var i = 0; i < 2000; i++) g.push([i, {}]);
f.name + ' ' + f.length + ' ' + f()
