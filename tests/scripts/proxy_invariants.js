// A proxy's trap results are checked against its target: a
// non-configurable property, or a non-extensible target, can't be
// reported otherwise than the target has it (a TypeError).
// expect: TypeError 1 TypeError false TypeError TypeError 0 TypeError TypeError TypeError TypeError TypeError ok TypeError TypeError

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
var frozen = Object.freeze({a: 1});
t(() => new Proxy(frozen, {get: () => 2}).a);
t(() => new Proxy(frozen, {get: () => 1}).a);
t(() => "a" in new Proxy(frozen, {has: () => false}));
t(() => "b" in new Proxy({}, {has: () => false}));
t(() => delete new Proxy(frozen, {deleteProperty: () => true}).a);
t(() => Object.keys(new Proxy(frozen, {ownKeys: () => []})));
t(() => Object.keys(new Proxy({x: 1}, {ownKeys: () => ["y"]})).length);
t(() => Object.getPrototypeOf(new Proxy(Object.preventExtensions({}), {getPrototypeOf: () => Array.prototype})));
t(() => Object.isExtensible(new Proxy({}, {isExtensible: () => false})));
t(() => Object.preventExtensions(new Proxy({}, {preventExtensions: () => true})));
t(() => Object.getOwnPropertyDescriptor(new Proxy(frozen, {getOwnPropertyDescriptor: () => undefined}), "a"));
t(() => Object.defineProperty(new Proxy({}, {defineProperty: () => true}), "a", {value: 1, configurable: false}));
t(() => { var p = new Proxy({}, {defineProperty: () => true}); Object.defineProperty(p, "a", {value: 1}); return "ok"; });
t(() => { "use strict"; var p = new Proxy(frozen, {set: () => true}); p.a = 3; return "ok"; });
t(() => Object.setPrototypeOf(new Proxy(Object.preventExtensions({}), {setPrototypeOf: () => true}), Array.prototype));
r.join(" ");
