// A `next` borrowed from another realm's iterator prototype runs in
// that realm (§10.3.1), so the entries it makes have that realm's
// `Array.prototype`: neither the protector's fast paths nor
// `step_with`'s shortcut may stand in for it (`symbol::is_native`
// checks the realm). Restored, the fast paths come back.
// expect: true false true true true true true 9 true

var g = $262.createRealm().global, out = [];
var MIP = Object.getPrototypeOf(new Map().entries()), mn = MIP.next;
MIP.next = Object.getPrototypeOf(new g.Map().entries()).next;
for (var e of new Map([[1, 2]])) out.push(Object.getPrototypeOf(e) === g.Array.prototype, Object.getPrototypeOf(e) === Array.prototype);
var [p] = new Map([[3, 4]]); out.push(Object.getPrototypeOf(p) === g.Array.prototype);
out.push(Object.getPrototypeOf([...new Map([[5, 6]])][0]) === g.Array.prototype);
MIP.next = mn;
var SIP = Object.getPrototypeOf(new Set().entries()), sn = SIP.next;
SIP.next = Object.getPrototypeOf(new g.Set().entries()).next;
for (var e of new Set([1]).entries()) out.push(Object.getPrototypeOf(e) === g.Array.prototype);
SIP.next = sn;
var AIP = Object.getPrototypeOf([].values()), an = AIP.next;
AIP.next = Object.getPrototypeOf(g.Array.prototype.values.call([])).next;
for (var e of [7].entries()) out.push(Object.getPrototypeOf(e) === g.Array.prototype);
out.push(Object.getPrototypeOf(Array.from([8].entries())[0]) === g.Array.prototype);
for (var x of [9]) out.push(x);
AIP.next = an;
for (var e of new Map([[1, 2]])) out.push(Object.getPrototypeOf(e) === Array.prototype);
out.join(" ")
