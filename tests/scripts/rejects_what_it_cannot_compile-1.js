// A tagged template can't follow an optional chain, but a template's
// continuation after a chain in a substitution isn't one.
// compile error: 2:9: unexpected `;`
var a = 1;
var b = ;
