// fib split over two mutually recursive functions.
// toi: direct calls between functions compiled together (mutual recursion)
// features: call, recursion
// expect: 196418
function f(n) { if (n < 2) return n; return g(n - 1) + g(n - 2); }
function g(n) { if (n < 2) return n; return f(n - 1) + f(n - 2); }
f(27);
