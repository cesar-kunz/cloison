// What crosses a `ShadowRealm`'s boundary thrown is a `TypeError`, its
// message made without running the realm's code (no `toString`, as
// V8): the caller's frames, live, stay so and its `catch` catches.
// expect: true,ShadowRealm evaluation threw: [object Object],42,true,ShadowRealm evaluation threw: RangeError: boom,true,ShadowRealm evaluation threw: str,42,42,true,undefined

var out = [], r = new ShadowRealm();
function f(s) { try { r.evaluate(s); } catch (e) { out.push(e.constructor === TypeError, e.message); } return 42; }
out.push(f("throw { toString() { globalThis.called = 1; return 'x'; } }"));
out.push(f("throw new RangeError('boom')"), f("throw 'str'"));
try { r.evaluate("(function () { throw { toString() {} }; })")(); } catch (e) { out.push(e.constructor === TypeError); }
out.push(r.evaluate("typeof called"));
out.join()
