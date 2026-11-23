// `Array.prototype.join` of a primitive holds the wrapper `this` becomes
// across the separator's conversion: a collection its `toString` causes
// leaves it alive, its elements read after.
// expect: a-b-c-d-e-f|a-b-c-d-e-f|a-b-c-d-e-f
var sep = { toString() { var junk = []; for (var i = 0; i < 1000; i++) junk.push({ i }); return "-"; } };
var out = [];
for (var k = 0; k < 3; k++) out.push(Array.prototype.join.call("abcdef", sep));
out.join("|");
