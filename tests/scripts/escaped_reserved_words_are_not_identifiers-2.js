// A reserved word spelled with escapes is no identifier.
// compile error: 1:11: `catch` (escaped) is a reserved word
var x = { c\u0061tch } = { catch: 42 };
