// A direct call whose callee varies (a table of handlers, as gbemu's
// opcodes): calls of the others through sites of their own in the site's
// table (`Dispatch`): closures with and without upvalues, made more than
// once (bound by code), one returning another type now and then, and
// handlers replaced.
// expect: 150551 34435236 11

function Cpu() { this.a = 0; this.b = 1; this.ops = []; }
function makeOps(cpu, k) {
  var ops = [];
  for (var i = 0; i < 40; i++) {
    if (i % 5 == 0) {
      var cell = { n: i };
      ops.push(function (c) { cell.n++; c.a = (c.a + cell.n + k) | 0; return 1; });
    } else if (i % 7 == 0) {
      ops.push(function (c) { c.b = c.b * 1.5; if (c.b > 1e6) c.b = 1; return 2; });
    } else {
      ops.push(new Function("c", "c.a = (c.a + " + i + ") | 0; return " + (i % 3) + ";"));
    }
  }
  ops[3] = function (c) { return c.a > 100000 ? "big" : 0; };
  ops[9] = function (c) { return ops[1](c) + ops[2](c); };
  return ops;
}
function run(cpu, ops, n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    var r = ops[(i * 7 + (i >> 3)) % ops.length](cpu);
    t += typeof r == "number" ? r : 5;
  }
  return t;
}
function main() {
  var cpu = new Cpu(), t = 0;
  var ops1 = makeOps(cpu, 1);
  for (var k = 0; k < 300; k++) {
    t += run(cpu, ops1, 400);
    if (k == 150) ops1[7] = function (c) { c.a = "s"; return 1; };
    if (k == 151) ops1[7] = function (c) { c.a = 0; return 1; };
    if (k % 100 == 99) ops1 = makeOps(cpu, k);
  }
  return t + " " + cpu.a + " " + Math.round(cpu.b);
}
main();
