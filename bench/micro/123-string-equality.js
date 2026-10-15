// === between strings: literals against strings built at run time.
// features: loop, string, array, call
// expect: 2100000
function lookupKind(name) {
  if (name === "alpha") return 1;
  if (name === "beta") return 2;
  if (name === "gamma") return 3;
  return 0;
}
function main() {
  var names = ["alp" + "ha", "be" + "ta", "gam" + "ma", "delta", "alpha", "x"];
  var built = [];
  for (var i = 0; i < names.length; i++) built.push(names[i].substring(0));
  var s = 0;
  for (var r = 0; r < 300000; r++)
    for (var i = 0; i < built.length; i++) s += lookupKind(built[i]);
  return s;
}
main();
