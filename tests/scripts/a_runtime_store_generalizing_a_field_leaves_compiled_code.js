// A store through the runtime that generalizes a field compiled code took
// the type of (a string into a field of numbers) leaves that code: the
// load after it doesn't take the string for a number.
// expect: number,2,true string,s,true object,[object Object],true number,1.5,true

function f0(b, n) {
  var o1 = { x: 1, y: 2.5 }, r = [];
  for (var i = 0; i < n; i++) {
    if (i) o1 = { x: b, w: 1 };
    o1.y = b;
    var v = o1.y;
    if (i == 0) r.push(typeof v, String(v), v === b);
  }
  return r.join(",");
}
for (var k = 0; k < 20; k++) f0(2, 2000);
[f0(2, 2000), f0("s", 2000), f0({}, 2000), f0(1.5, 2000)].join(" ");
