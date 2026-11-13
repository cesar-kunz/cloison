// A tagged template can't follow an optional chain, but a template's
// continuation after a chain in a substitution isn't one.
// compile error: 1:13: no target for `continue`
while (1) { continue x; }
