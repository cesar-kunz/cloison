// A reserved word spelled with escapes is no identifier.
// expect: 3
var \u0061b = 1; var o = { \u0069f: 2 }; l\u0065t: 1; ab + o.if;
