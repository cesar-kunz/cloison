// Strings of other lengths are unequal without flattening them.
// expect: true true false false false true false false true false false true true true true false true false A BC A 65535

var r = [];
var a = "ab"; a += "c";
var b = "a"; b += "bc";
var keys = ["abc", "abd", "ab", "abcd"];
for (var i = 0; i < keys.length; i++) r.push(a === keys[i], a == keys[i], keys[i] !== b);
r.push(a === b, a == b, "1" == 1, a == 3, "" == 0, null == "");
r.push(String.fromCharCode(65), String.fromCharCode(66, 67), String.fromCharCode(65.7), String.fromCharCode(-1).charCodeAt(0));
r.join(" ");
