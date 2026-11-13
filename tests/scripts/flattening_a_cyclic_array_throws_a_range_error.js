// `flat(Infinity)` of an array holding itself: a RangeError.
// expect: RangeError
var a = []; a[0] = a; var r; try { a.flat(Infinity); r = 'no error'; } catch (e) { r = e.name; } r
