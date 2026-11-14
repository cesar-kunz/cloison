// A chain of a hundred iterator helpers, natives calling natives with no
// bytecode frame between, fits the natives' stack: the bound that makes
// a deeper one a `RangeError` (deep_native_recursion_is_a_range_error.js,
// whose chain is of fifty, for a debug build's sake) leaves room for it.
// release only: a debug build's frames run out before a hundred helpers.
// expect: 101,102

var it = [1, 2].values();
for (var i = 0; i < 100; i++) it = it.map(x => x + 1);
[...it].join();
