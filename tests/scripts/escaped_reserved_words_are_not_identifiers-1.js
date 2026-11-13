// A reserved word spelled with escapes is no identifier.
// compile error: 1:12: `break` (escaped) is a reserved word
var x = ({ bre\u0061k }) => {};
