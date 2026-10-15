// An if/else-if chain over int ranges (a dispatch on a value's magnitude).
// features: loop, call
// expect: 1960391
function grade(score) {
  if (score >= 90) return 4;
  else if (score >= 80) return 3;
  else if (score >= 70) return 2;
  else if (score >= 60) return 1;
  else if (score < 0) return -1;
  return 0;
}
function main() {
  var s = 0;
  for (var i = 0; i < 2000000; i++) s += grade((i * 7) % 101 - 1);
  return s;
}
main();
