// A method loop re-reading this.items, its length and this.scale on every iteration.
// features: loop, object, new, proto, array, call
// expect: 29820000
function Series(n) {
  this.items = [];
  this.scale = 3;
  for (var i = 0; i < n; i++) this.items.push(i % 11);
}
Series.prototype.weighted = function () {
  var s = 0;
  for (var i = 0; i < this.items.length; i++) s += this.items[i] * this.scale;
  return s;
};
function main() {
  var ser = new Series(500), t = 0;
  for (var r = 0; r < 4000; r++) t += ser.weighted();
  return t;
}
main();
