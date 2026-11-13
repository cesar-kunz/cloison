// A reserved word spelled with escapes is no identifier.
// compile error: 1:5: `var` (escaped) is a reserved word
var v\u0061r = 1;
