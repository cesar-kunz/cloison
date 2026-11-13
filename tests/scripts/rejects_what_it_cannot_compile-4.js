// A tagged template can't follow an optional chain, but a template's
// continuation after a chain in a substitution isn't one.
// compile error: 2:1: a tagged template in an optional chain
var x; x?.y
`t`;
