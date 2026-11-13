// An arrow function's own `"use strict"` makes it strict.
// expect: strict
var g = () => { "use strict"; try { undeclared = 1; return "sloppy"; } catch (e) { return "strict"; } }; g();
