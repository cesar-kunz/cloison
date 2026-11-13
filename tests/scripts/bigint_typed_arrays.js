// `BigInt64Array`, `BigUint64Array` and `DataView`'s BigInt methods:
// elements converted with ToBigInt (a number is a TypeError), read as
// BigInts, through the builtins and in hot code.
// expect: -1,-9223372036854775808,5,7 18446744073709551615,9223372036854775808,5,7 TypeError 1,2,1 -9223372036854775808,5|5,7|-2,0,10,14 -1,2,3 -1,-9223372036854775808,9,9 TypeError 2|true|1,2|4,-9223372036854775808,9,9 -2,18446744073709551614,-2,-2 TypeError 14845050

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
var a = new BigInt64Array(4);
a[0] = -1n; a[1] = 2n ** 63n; a[2] = 5n; a[3] = 2n ** 64n + 7n;
t(() => a.join()); t(() => new BigUint64Array(a.buffer).join());
t(() => { a[0] = 1; }); t(() => new BigInt64Array([1n, "2", true]).join());
t(() => [a.slice(1, 3).join(), a.filter(x => x > 0n).join(), a.map(x => x * 2n).join()].join("|"));
t(() => new BigInt64Array([3n, -1n, 2n]).sort().join()); t(() => a.fill(9n, 2).join()); t(() => a.fill(1));
t(() => [a.indexOf(9n), a.includes(9n), BigInt64Array.of(1n, 2n).join(), a.with(0, 4n).join()].join("|"));
var dv = new DataView(new ArrayBuffer(16));
dv.setBigInt64(0, -2n); dv.setBigUint64(8, 2n ** 64n - 2n, true);
t(() => [dv.getBigInt64(0), dv.getBigUint64(0), dv.getBigInt64(8, true), dv.getInt32(4)].join());
t(() => dv.setBigInt64(0, 1));
var big = new BigInt64Array(100), s = 0n;
for (var k = 0; k < 3000; k++) { for (var i = 0; i < 100; i++) big[i] = BigInt(i) * BigInt(k); }
for (var i = 0; i < 100; i++) s += big[i];
t(() => s);
r.join(" ");
