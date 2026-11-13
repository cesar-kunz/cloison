// Compiled code's store of an object into a typed array calls its
// `valueOf` each time: the runtime call that can't call it leaves the
// store to the interpreter, rather than storing NaN (cloison-fuzz
// BUGS.md #18).
// expect: 312 7 7 5 TypeError

var n = 0;
var cnt = { valueOf() { n++; return 7; } };
var u32 = new Uint32Array(4), f64 = new Float64Array(4), b64 = new BigInt64Array(4);
function f(v) { u32[1] = v; f64[1] = v; }
function g(v) { b64[1] = v; }
function loop1() { for (var i = 0; i < 200; i++) f(i < 44 ? i : cnt); }
function loop2() { for (var i = 0; i < 200; i++) g(i < 44 ? BigInt(i) : 5n); }
function sym() { try { f(Symbol()); } catch (x) { return x.constructor.name; } }
loop1(); loop2();
[n, u32[1], f64[1], b64[1], sym()].join(' ');
