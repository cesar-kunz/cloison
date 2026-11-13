// Iterator results get the shape the realm's first one got
// (`Vm::iter_result_shape`), and %ArrayIteratorPrototype%.next steps a
// dense array directly. The results are ordinary objects: their keys in
// order, writable, enumerable, configurable, changeable, of
// `Object.prototype`; holes read through the prototype, an array that
// grows or shrinks while iterated, typed arrays, array-likes and
// subclasses; a patched `next` runs for for-of and destructuring.
// Expected values from V8 and SpiderMonkey.
// expect: [{"value":1,"done":false},{"done":false},{"value":3,"done":false},{"done":true}] ; value|done ; true/true/true ; true/true/true ; true ; 0,p,2 ; {"value":9,"done":false,"extra":1} ; {"done":false} ; {"done":true} ; true ; {"value":3,"done":5} ; true/true/true ; {"value":7,"done":false} ; {"value":2,"done":false} ; {"done":true} ; [1.5,"x",{"k":1},null,null] ; [0,1] ; [[0,5],[1,6]] ; {"value":[1,2],"done":false} ; {"value":"a","done":false} ; value|done ; true/true/true ; 60 ; 90 ; 6 ; {"value":10,"done":false} ; [7,8] ; {"value":"a","done":false} ; {"value":4,"done":false} ; {"done":true}

var out = [];
function d(o, k) { var x = Object.getOwnPropertyDescriptor(o, k); return x ? [x.writable, x.enumerable, x.configurable].join('/') : 'none'; }
var it = [1, , 3][Symbol.iterator](), r1 = it.next(), r2 = it.next(), r3 = it.next(), r4 = it.next();
out.push(JSON.stringify([r1, r2, r3, r4]), Reflect.ownKeys(r1).join('|'), d(r1, 'value'), d(r4, 'done'), Object.getPrototypeOf(r4) === Object.prototype);
Array.prototype[1] = 'p'; out.push([...[0, , 2]].join()); delete Array.prototype[1];
r1.value = 9; r1.extra = 1; delete r2.value; out.push(JSON.stringify(r1), JSON.stringify(r2), JSON.stringify(it.next()), r1 !== it.next());
Object.defineProperty(r3, 'done', { value: 5 }); out.push(JSON.stringify(r3), d(r3, 'done'), JSON.stringify([7].values().next()));
var a = [1, 2]; it = a.values(); it.next(); a.push(3); a.length = 2; out.push(JSON.stringify(it.next()), JSON.stringify(it.next()));
var f = [1.5, 'x', { k: 1 }, null, undefined]; out.push(JSON.stringify(Array.from(f.values())));
out.push(JSON.stringify([...[5, 6].keys()]), JSON.stringify([...[5, 6].entries()]), JSON.stringify(new Map([[1, 2]]).entries().next()), JSON.stringify('ab'[Symbol.iterator]().next()));
function* g() { yield 1; } var gr = g().next(); out.push(Reflect.ownKeys(gr).join('|'), d(gr, 'value'));
var AIP = Object.getPrototypeOf([].values()), next = AIP.next, calls = 0;
AIP.next = function () { calls++; var r = next.call(this); return { value: r.done ? r.value : r.value * 10, done: r.done }; };
var s = 0; for (var x of [1, 2, 3]) s += x; var [p, q] = [4, 5];
out.push(s, p + q, calls, JSON.stringify([1, 2].values().next()));
AIP.next = next;
var ta = new Uint8Array([7, 8]); out.push(JSON.stringify([...ta.values()]));
var ar = { length: 2, 0: 'a', 1: 'b' }; out.push(JSON.stringify(Array.prototype.values.call(ar).next()));
var sub = new (class extends Array {})(); sub.push(4); out.push(JSON.stringify(sub.values().next()));
var big = []; big[5] = 1; it = big.values(); for (var k = 0; k < 6; k++) it.next(); out.push(JSON.stringify(it.next()));
out.join(' ; ')
