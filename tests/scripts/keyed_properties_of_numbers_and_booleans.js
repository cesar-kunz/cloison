// A number's or a boolean's property by a computed key is its
// prototype's (`(5)["toString"]`), an index too, the primitive a getter's
// `this`.
// expect: 5 1.50 false true 3.142 n0  b1 n0 number 4999

var r = [(5)["toString"](), (1.5)["toFixed"](2), false["toString"](), true["valueOf"]()];
var k = "toFixed"; r.push((3.14159)[k](3));
Number.prototype[0] = "n0"; Boolean.prototype[1] = "b1";
r.push((7)[0], (7)[1], true[1], (2.5)["0"]);
Object.defineProperty(Number.prototype, 3, { get() { "use strict"; return typeof this; } });
r.push((4)[3]);
function hot(n) { var s = ""; for (var i = 0; i < n; i++) s = (i)["toString"](); return s; }
r.push(hot(5000));
r.join(" ");
