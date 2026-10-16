// A field written in the constructor and never read; its value is a fresh string.
// cloison: for internal objects the field and the concatenation can be dropped.
// features: loop, object, new, string
// expect: 153434128
function V(name, value) { this.value = value; this.name = name; }
function main() {
  var sum = 0;
  for (var i = 0; i < 300000; i++) {
    var v = new V("v" + i, i & 1023);
    sum += v.value;
  }
  return sum;
}
main();
