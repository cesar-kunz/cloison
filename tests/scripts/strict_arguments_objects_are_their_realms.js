// A realm's strict `arguments` objects are of its own prototype and
// shape. Expected values from V8 (SpiderMonkey's shell has no
// `ShadowRealm`).
// expect-json: "0|1|length|callee|Symbol(Symbol.iterator):true:0,1 ; 1 ; "

'use strict';
var out = [];
function f() { return arguments; }
var r = new ShadowRealm();
out.push(r.evaluate('"use strict"; function f() { return arguments; } var a = f(1), b = f(2, 3); Reflect.ownKeys(b).map(String).join("|") + ":" + (Object.getPrototypeOf(b) === Object.prototype) + ":" + Object.keys(b)'));
out.push(r.evaluate('"use strict"; Object.prototype.z = 1; (function () { return arguments.z; })()'), f().z);
out.join(' ; ')
