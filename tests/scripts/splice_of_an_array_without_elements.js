// Issue #277: an array literal without elements has none allocated
// (`elems` null), and a `splice` that leaves it empty fills no slots past
// its end: not a slice of them (the debug build aborted, as `reverse`'s).
// expect: 0,0,0,0,0,0
var a = [], b = [], c = new Array(0);
[a.splice(0, 0).length, a.length, b.splice().length, b.length, c.splice(5, 3).length, c.length].join()
