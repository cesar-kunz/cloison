// A leaf native of another realm is called through the general path (it
// runs in its realm), and a realm's replaced `Math.pow` or `charCodeAt`
// is the function called there only. Expected values from V8
// (SpiderMonkey's shell has no `ShadowRealm`).
// expect: 1024,99,HI,true,69,8,97,8,35,-7

var out = [], r = new ShadowRealm();
var rp = r.evaluate('Math.pow'), rc = r.evaluate('(s, i) => s.charCodeAt(i)'), rf = r.evaluate('String.fromCharCode'), rm = r.evaluate('Math.max');
out.push(rp(2, 10), rc('abc', 2), rf(72, 73), rm(-0, 0) === 0, r.evaluate('Math.max(3, 4) + String.fromCharCode(65).charCodeAt(0)'));
r.evaluate('Math.pow = function () { return 42; }; String.prototype.charCodeAt = function () { return -7; }; 0');
out.push(Math.pow(2, 3), 'a'.charCodeAt(0), rp(2, 3), r.evaluate('Math.pow(2, 3) + "a".charCodeAt(0)'), rc('abc', 0));
out.join()
