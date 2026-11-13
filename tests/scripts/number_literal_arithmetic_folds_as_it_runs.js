// Codegen folds operators applied to number literals (`num_const`) as
// the run time computes them, and the `**` Intrinsic computes numbers'
// powers in the handler: -0, NaN and infinities, exponents past the
// double range and below the smallest subnormal, int32 overflow and the
// shifts' and bitwise operators' ToInt32, right associativity, and
// operands that convert (valueOf, strings, null, BigInts). Expected
// values from V8 and SpiderMonkey.
// expect: 2147483648,2147483647,1608260661,4.656612873077393e-10,-1.4142135623730951,5e-324,0,Infinity,Infinity,true,NaN,1,1,NaN,NaN,-Infinity,Infinity,-Infinity,-Infinity,NaN,1.5,-1,-Infinity,-2147483648,-2147483649,2147483648,-2147483649,4294967294,2147483648,2147483648,4294967296,4294967295,1,2,0,5,-559939584,-1,-2147483648,81,-8,512,64,3.3333333333333335,0.30000000000000004,Infinity,-Infinity,1,9007199254740992,9007199254740992,27,8,0.421875,0.3333333333333333,NaN,81,-Infinity,Infinity,8,9,4,1,NaN,18446744073709551616,-8,RangeError,TypeError,1024,number,true

var x = 3, out = [];
out.push(2 ** 31, 2 ** 31 - 1, (2 ** 31 - 1) & 12345678901, 1 / 2 ** 31, -(2 ** 0.5), 2 ** -1074, 2 ** -1075, 2 ** 1023 * 2, 2 ** 1024, (-2) ** 1023 < 0);
out.push((-8) ** (1 / 3), 0 ** 0, NaN ** 0, 1 ** NaN, (-1) ** Infinity, 1 / (0 * -1), 1 / (-0 + 0), 1 / -(1 - 1), 1 / (0 / -5), 5 % -0, 7.5 % -2, -7 % 2, 1 / (-4 % 2));
out.push(1 << 31, (1 << 31) - 1, 2147483647 + 1, -2147483648 - 1, 2147483647 * 2, -(-2147483648), -2147483648 * -1, 65536 * 65536, -1 >>> 0, 1 << 32, 1 << 33, 4294967296 | 0, (2 ** 32 + 5) | 0, 1e21 | 0, -1 >> 31, 0x80000000 >> 0);
out.push(3 ** 2 ** 2, (-2) ** 3, 2 ** 3 ** 2, (2 ** 3) ** 2, 10 / 3, 0.1 + 0.2, 1e308 * 10, -1e308 * 10, 1 / 3 * 3, 9007199254740993 + 0, 2 ** 53 + 1);
out.push(x ** 3, 2 ** x, (1 - 0.25) ** 3, x ** -1, (-x) ** 0.5, x ** 2 ** 2, 1 / (-0) ** 3, 1 / (-0) ** 2);
var o = { valueOf() { return 2; } }; out.push(o ** 3, 3 ** o, '2' ** 2, null ** 0, undefined ** 1, 2n ** 64n, (-2n) ** 3n);
try { 2n ** -1n; } catch (e) { out.push(e.name); }
try { 2 ** 1n; } catch (e) { out.push(e.name); }
var y = 2; y **= 10; out.push(y, typeof (2 ** 31), Object.is((-0) ** 1, -0));
out.join()
