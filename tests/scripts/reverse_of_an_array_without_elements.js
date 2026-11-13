// An array literal without elements has none allocated (`elems` null),
// which `reverse` must not make a slice of (the debug build aborted).
// expect: 0
var a = []; a.reverse() === a && a.length
