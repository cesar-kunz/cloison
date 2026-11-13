// Each realm's iterator results have its own `Object.prototype` (the
// shape is a realm's field). Expected values from V8 (SpiderMonkey's
// shell has no `ShadowRealm`).
// expect: {"value":1,"done":false}value|done ; 3 ;  ; "{\"value\":2,\"done\":false}"

var out = [];
var r = new ShadowRealm();
out.push(r.evaluate('var i = [1, 2].values(), x = i.next(); Object.getPrototypeOf(x) === Object.prototype && JSON.stringify(x) + Reflect.ownKeys(x).join("|")'));
out.push(r.evaluate('Object.prototype.extra = 3; [1].values().next().extra'), [1].values().next().extra, JSON.stringify(r.evaluate('JSON.stringify([2].values().next())')));
out.join(' ; ')
