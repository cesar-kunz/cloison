// `await` is no identifier in a class static block's own code (but in a
// function's or an arrow's in it, and as a function expression's name).
// compile error: 1:20: `await` as an identifier here
class C { static { await; } }
