// `slice` and `splice` convert their arguments, which may change the
// array's length, after reading it: their fast paths must not use
// indices measured against the old one (before, `splice` copied past
// the elements' end).
// expect: 100 5  1 99 9

var a = []; for (var i = 0; i < 100; i++) a[i] = { i: i };
var s = a.slice(0, { valueOf: function () { a.length = 10; return 100; } });
var b = []; for (var i = 0; i < 100; i++) b[i] = i;
var r = b.splice({ valueOf: function () { b.length = 10; return 50; } }, 1);
[s.length, s[5].i, s[50], r.length, b.length, b[9]].join(" ");
