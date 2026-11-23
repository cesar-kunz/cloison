// `String.raw` holds the `raw` object across its length's and pieces'
// conversions: one a getter just made, which only it refers to, stays
// alive through the collections they cause.
// expect: a-b+c|a-b+c|a-b+c
var made = 0;
var tpl = { get raw() {
  var r = { 0: "a", 1: "b", 2: "c" };
  for (var i = 0; i < 50; i++) r["p" + i] = { x: i };
  r.length = { valueOf() { var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i }); return 3; } };
  made++;
  return r;
} };
var out = [];
for (var k = 0; k < 3; k++) out.push(String.raw(tpl, "-", "+"));
out.join("|");
