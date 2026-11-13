// Math's functions, called with no cell argument, run as leaf natives
// (`Code::leaf`, `interp::leaf_call`): directly, without `call_other`'s
// entry; the string methods reading a unit, which may flatten or
// allocate, are called generally. Their results with every
// argument type, `this` of the wrong type (a number, a boolean, an
// object converted; null and undefined throw), argument conversions in
// order (valueOf called once each, the first throw wins over a symbol's
// TypeError), -0 and NaN, and BigInts rejected, are the general path's.
// Expected values from V8 and SpiderMonkey.
// expect: 233,NaN,NaN,104,233,233,104,104,128512,56832,,true,true,h,50,116,.,81,TypeError,TypeError,4,,0,A,a,9,5,2,3,2,NaN,2,true,4,abbabaabbb,120,abbabaabbbi,TypeError,RangeError,Infinity,-Infinity,-Infinity,Infinity,0,-Infinity,0,3.141592653589793,5.5,31,-4,-2,2,number,true,TypeError

var out = [], log = [];
var s = 'h\u00e9llo\ud83d\ude00';
out.push(s.charCodeAt(1), s.charCodeAt(99), s.charCodeAt(-1), s.charCodeAt(), s.charCodeAt(1.9), s.charCodeAt(true), s.charCodeAt(null), s.charCodeAt(NaN));
out.push(s.codePointAt(5), s.codePointAt(6), s.codePointAt(7), s.charAt(1) === '\u00e9', s.charAt(-1) === '', s.charAt(undefined));
var cc = String.prototype.charCodeAt, cp = String.prototype.codePointAt, ca = String.prototype.charAt;
out.push(cc.call(12345, 1), cp.call(true, 0), ca.call(98.5, 2), cc.call({ toString() { return 'Q'; } }, 0));
try { cc.call(null, 0); } catch (e) { out.push(e.name); }
try { ca.call(undefined); } catch (e) { out.push(e.name); }
out.push(String.fromCharCode(65, 66.7, -1 >>> 16, 65536 + 67).length, String.fromCharCode(), String.fromCharCode(undefined).charCodeAt(0), String.fromCharCode(0x10041));
var fcc = String.fromCharCode, pow = Math.pow, max = Math.max;
out.push(fcc(97), pow(3, 2), max(1, 5), Math.min.call(null, 4, 2), Math.abs.apply(null, [-3]));
var v1 = { valueOf() { log.push('a'); return 1; } }, v2 = { valueOf() { log.push('b'); return 2; } };
out.push(Math.max(v1, v2), Math.min(v2, NaN, v1), Math.pow(v2, v1), Math.hypot(v1, v2) === Math.sqrt(5), Math.imul(v2, v2), log.join(''));
out.push('x'.charCodeAt({ valueOf() { log.push('i'); return 0; } }), log.join(''));
try { Math.max(1, Symbol()); } catch (e) { out.push(e.name); }
try { Math.max({ valueOf() { throw new RangeError('r'); } }, Symbol()); } catch (e) { out.push(e.name); }
out.push(1 / Math.max(-0, 0), 1 / Math.min(0, -0), Math.max(), Math.min(), Math.round(-0.5), 1 / Math.round(-0.4), Math.sign(-0), Math.atan2(0, -0), Math.fround(5.5), Math.clz32(1), Math.trunc(-4.7), Math.cbrt(-8));
out.push(Math.max(1n === 1n ? 2 : 0, 1), typeof Math.random(), Math.random() < 1);
try { Math.abs(1n); } catch (e) { out.push(e.name); }
out.join()
