// BigInts: literals, the operators (a number mixed in is a TypeError),
// comparisons and equality with numbers and strings, by value as Map
// keys, `++`/`--` (not `x - 1`'s `SubSmi`), the builtins, and the same
// in hot code the JIT compiles for numbers first.
// expect: 1000000000000000000000000000000 -3 -1 1051 bigint true,false,true,true,true,true,0 10,16,-1,18446744073709551615,ff -3,40,2,7,5,-6,5 4,5,4,5 TypeError RangeError TypeError TypeError TypeError TypeError TypeError TypeError RangeError 18446744073709552000,true,true,a,1 2,object,[object BigInt],6 121932631137021795226185032733622923332237463801111263526900 bnklg118comha6gqury14067gur54n8won6guf4 14,true,false,4 TypeError 354224848179261915075

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
t(() => 10n ** 30n); t(() => -7n / 2n); t(() => -7n % 2n); t(() => 0x1fn + 0b101n + 0o17n + 1_000n);
t(() => typeof 1n); t(() => [1n == 1, 1n === 1, 1n == "1", 2n > 1, 1n < 1.5, "2" > 1n, 0n ? 1 : 0].join());
t(() => [BigInt(10), BigInt("0x10"), BigInt.asIntN(8, 255n), BigInt.asUintN(64, -1n), (255n).toString(16)].join());
t(() => [-5n >> 1n, 5n << 3n, -6n & 3n, 6n | 1n, 6n ^ 3n, ~5n, -(-5n)].join());
t(() => { var x = 5n; x++; x--; x--; var y = x++; return [y, x, --x, ++x].join(); });
t(() => 1n + 1); t(() => 1n / 0n); t(() => 1n >>> 0n); t(() => +1n); t(() => 5n - 1); t(() => 5n & 1);
t(() => JSON.stringify({a: 1n})); t(() => new BigInt(1)); t(() => BigInt(1.5));
t(() => [Number(2n ** 64n), Object.is(1n, 1n), [1n, 2n].includes(2n), new Map([[1n, "a"]]).get(1n), new Set([1n, 1n]).size].join());
t(() => [Object(1n) + 1n, typeof Object(1n), Object.prototype.toString.call(1n), {valueOf() { return 3n; }} * 2n].join());
t(() => 123456789012345678901234567890n * 987654321098765432109876543210n);
t(() => (2n ** 200n).toString(36));
function sub(a, b) { return a - b; }
function eq(a, b) { return a === b; }
function dec(a) { a--; return a; }
function m1(a) { return a - 1; }
for (var k = 0; k < 20000; k++) { sub(k, 3); eq(k, 5); dec(k); m1(k); }
t(() => [sub(17n, 3n), eq(5n, 5n), eq(5n, 6n), dec(5n)].join()); t(() => m1(5n));
t(() => { var a = 0n, b = 1n; for (var i = 0; i < 100; i++) { var c = a + b; a = b; b = c; } return a; });
r.join(" ");
