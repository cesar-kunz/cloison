// Storing numbers into an object reachable from a global (an external object).
// features: loop, object, new, global
// expect: 1000575
var state = new Object();
state.mark = 0;
state.last = 0;
function main() {
  for (var i = 0; i < 1000000; i++) {
    state.mark = state.mark + 1;
    state.last = i & 1023;
  }
  return state.mark + state.last;
}
main();
