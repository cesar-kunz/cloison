// As for arrays (see `array_iteration_protector_is_per_realm` and
// `array_iteration_protector_sees_proxies`): a map's, a set's and a
// string's iteration by the builtins, under the protector's bits.
// expect: 33,return,return,return

var r = new ShadowRealm(), out = [], c = 0;
r.evaluate(`for (const x of new Map([[1, 1]])) {}; for (const x of new Set([1])) {}; for (const x of "a") {}; 0`);
var MIP = Object.getPrototypeOf(new Map().entries()), mn = MIP.next;
MIP.next = function () { c++; return mn.call(this); };
for (const x of new Map([[1, 1], [2, 2]])) {}
MIP.next = mn;
var SIP = Object.getPrototypeOf(""[Symbol.iterator]()), sn = SIP.next;
SIP.next = function () { c += 10; return sn.call(this); };
for (const x of "ab") {}
SIP.next = sn;
out.push(c);
var IP = Object.getPrototypeOf(MIP), old = Object.getPrototypeOf(IP), seen = [];
Object.setPrototypeOf(IP, new Proxy({}, { get(t, k) { seen.push(String(k)); } }));
for (const x of "ab") break;
var [m] = new Map([[1, 1], [2, 2]]);
var [s] = new Set([1, 2]);
Object.setPrototypeOf(IP, old);
out.push(seen.join());
out.join()
