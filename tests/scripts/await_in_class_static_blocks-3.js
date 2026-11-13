// `await` is no identifier in a class static block's own code (but in a
// function's or an arrow's in it, and as a function expression's name).
// expect: 2

class C { static { (() => { let await = 1; })(); (function await() {}); this.ok = (function () { var await = 2; return await; })(); } }
C.ok;
