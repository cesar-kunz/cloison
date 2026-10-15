// Sieve of Eratosthenes on an array grown by indexed stores of booleans.
// features: loop, array, call
// expect: 226695
function sieve(n) {
  var composite = [];
  for (var i = 0; i <= n; i++) composite[i] = false;
  var count = 0;
  for (var i = 2; i <= n; i++) {
    if (composite[i]) continue;
    count++;
    for (var j = i + i; j <= n; j += i) composite[j] = true;
  }
  return count;
}
function main() {
  var s = 0;
  for (var r = 0; r < 100; r++) s += sieve(20000 + r);
  return s;
}
main();
