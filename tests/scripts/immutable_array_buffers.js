// Immutable `ArrayBuffer`s: `transferToImmutable` and
// `sliceToImmutable` make them; no view writes them, interpreted or
// compiled, and what would (methods, `DataView`, a strict store) throws.
// expect: true true 6 1,2,3,4,0,0 | TypeError | TypeError | TypeError | TypeError | TypeError | {"value":1,"writable":false,"enumerable":true,"configurable":false} | false,true | 3 | true 6,7 6,7 | TypeError | 140000 1,2,3,4,0,0

"use strict";
var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
var src = new Uint8Array([1, 2, 3, 4]).buffer;
var im = src.transferToImmutable(6);
var ta = new Uint8Array(im);
t(() => [src.detached, im.immutable, im.byteLength, ta.join()].join(" "));
t(() => { ta[0] = 9; });
t(() => ta.fill(0));
t(() => new DataView(im).setUint8(0, 1));
t(() => im.transfer());
t(() => im.resize(1));
t(() => JSON.stringify(Object.getOwnPropertyDescriptor(ta, 0)));
t(() => [Object.isFrozen(ta), Object.isFrozen(Object.preventExtensions(ta))].join());
t(() => new Uint8Array(im, 1, 2).subarray(1).join());
var sl = new Uint8Array([5, 6, 7, 8]).buffer.sliceToImmutable(1, -1);
t(() => [sl.immutable, new Uint8Array(sl).join(), new Uint8Array(sl.slice(0)).join()].join(" "));
// A compiled store tried on an immutable view after mutable ones.
function store(a, i) { a[i] = 7; return a[i]; }
var m = new Uint8Array(4), seen = 0;
for (var i = 0; i < 20000; i++) seen += store(m, i & 3);
t(() => store(ta, 1));
t(() => [seen, ta.join()].join(" "));
r.join(" | ");
