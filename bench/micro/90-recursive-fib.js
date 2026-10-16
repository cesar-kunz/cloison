// Doubly recursive fib: every call is a call, nothing to inline but itself.
// cloison: direct self-calls with predicted int32 results, inlined once into itself
// features: call, recursion
// expect: 196418
function fib(n) { if (n < 2) return n; return fib(n - 1) + fib(n - 2); }
fib(27);
