// Chained assignment storing one value into two objects' fields.
// features: loop, object, new
// expect: 254987712
function V() { this.mark = 0; }
function C() { this.scale = new V(); this.offset = new V(); }
function main() {
  var c = new C(), sum = 0;
  for (var i = 0; i < 1000000; i++) {
    c.scale.mark = c.offset.mark = i & 255;
    sum += c.scale.mark + c.offset.mark;
  }
  return sum;
}
main();
