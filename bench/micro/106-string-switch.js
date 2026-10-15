// `switch` over string tokens (an interpreter's opcode names).
// features: loop, string, switch, array, call
// expect: 1451971
function evalToken(t, acc) {
  switch (t) {
    case "inc": return acc + 1;
    case "dec": return acc - 1;
    case "dbl": return acc * 2;
    case "half": return acc >> 1;
    case "neg": return -acc;
    default: return acc;
  }
}
function main() {
  var prog = ["inc", "dbl", "inc", "half", "dec", "nop", "neg", "inc", "dbl", "neg"];
  var acc = 0, sum = 0;
  for (var r = 0; r < 200000; r++) {
    for (var i = 0; i < prog.length; i++) acc = evalToken(prog[i], acc) & 0xffff;
    sum += acc + r % 3;
  }
  return sum;
}
main();
