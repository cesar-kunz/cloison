// An array whose `length` is read-only doesn't grow, by a store, `Reflect`,
// `defineProperty` or a method, which then leaves it as it was; one a
// non-configurable element keeps from shrinking makes strict code throw.
// expect: false 3 TypeError TypeError TypeError TypeError [1,2,3] TypeError 2 undefined

var r = [];
function t(f) { try { return String(f()); } catch (e) { return e.name; } }
var a = [1, 2, 3]; Object.defineProperty(a, "length", { writable: false });
r.push(Reflect.set(a, 3, 9), a.length, t(() => Object.defineProperty(a, 3, { value: 1 })));
r.push(t(() => a.unshift(0)), t(() => a.splice(1, 0, 9)), t(() => a.push(4)), JSON.stringify(a));
var b = [1, 2, 3, 4]; Object.defineProperty(b, 1, { value: 2, configurable: false });
r.push(t(() => { "use strict"; b.length = 0; }), b.length, t(() => { b.length = 0; }));
r.join(" ");
