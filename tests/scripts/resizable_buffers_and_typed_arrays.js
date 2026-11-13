// Resizable buffers (views tracking their length, or out of bounds once
// it shrinks past them, and back), `transfer`, `DataView`s on them; the
// typed arrays' methods as the spec has them (species, `set` from a
// typed array of another kind); no own `length`, and canonical numeric
// keys that are no indices are none.
// expect: true,16,8,4,9 ; 0,0,0, ; TypeError ; 2,3 ; 7,7,0,0 ; RangeError ; TypeError ; true,0,0,8,5,false ; true,false ; TypeError ; 10,1 ; TypeError ; 10,-6,20 5,10 -3,10 -3,10 -3,5,10 1,-3,10 10 true 1 5|-3|10 ; 44,4,1,2 ; ,,false,false,true ; true,true,MyTA

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
t(() => { var b = new ArrayBuffer(4, { maxByteLength: 16 }); var ta = new Uint8Array(b); var fixed = new Uint8Array(b, 0, 4); b.resize(8); ta[7] = 9; return [b.resizable, b.maxByteLength, ta.length, fixed.length, ta[7]].join(); });
t(() => { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var fixed = new Uint16Array(b, 4, 2); b.resize(6); return [fixed.length, fixed.byteOffset, fixed.byteLength, fixed[0]].join(); });
t(() => { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var fixed = new Uint16Array(b, 4, 2); b.resize(6); return fixed.fill(1); });
t(() => { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var fixed = new Uint16Array(b, 4, 2); b.resize(6); b.resize(8); fixed[1] = 3; return [fixed.length, fixed[1]].join(); });
t(() => { var b = new ArrayBuffer(4, { maxByteLength: 8 }); new Uint8Array(b).fill(7); b.resize(2); b.resize(4); return [...new Uint8Array(b)].join(); });
t(() => { var b = new ArrayBuffer(4, { maxByteLength: 8 }); b.resize(9); });
t(() => new ArrayBuffer(4).resize(2));
t(() => { var b = new ArrayBuffer(4); var u = new Uint8Array(b); u[0] = 5; var c = b.transfer(8); return [b.detached, b.byteLength, u.length, c.byteLength, new Uint8Array(c)[0], c.resizable].join(); });
t(() => { var b = new ArrayBuffer(4, { maxByteLength: 8 }); return [b.transfer().resizable, new ArrayBuffer(2, { maxByteLength: 4 }).transferToFixedLength().resizable].join(); });
t(() => { var b = new ArrayBuffer(4); b.transfer(); return b.slice(0); });
t(() => { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var dv = new DataView(b, 2); b.resize(12); dv.setUint8(9, 1); return [dv.byteLength, dv.getUint8(9)].join(); });
t(() => { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var dv = new DataView(b, 6); b.resize(4); return dv.byteLength; });
t(() => { var a = new Int16Array([5, -3, 10]); return [a.map(x => x * 2).join(), a.filter(x => x > 0).join(), a.slice(1).join(), a.subarray(1).join(), a.toSorted().join(), a.with(0, 1).join(), a.at(-1), a.includes(10), a.indexOf(-3), a.join("|")].join(" "); });
t(() => { var a = new Uint8Array(4); a.set([1, 2], 2); a.set(new Uint16Array([300, 4])); return a.join(); });
t(() => { var a = new Float64Array(2); return [Object.getOwnPropertyDescriptor(a, "length"), a["-0"], "1.5" in a, Reflect.defineProperty(a, "-0", { value: 1 }), delete a["-0"]].join(); });
t(() => { class MyTA extends Uint8Array {} var m = new MyTA(4); return [m.map(x => x) instanceof MyTA, m.subarray(1) instanceof MyTA, m.slice(1).constructor.name].join(); });
r.join(" ; ");
