// Constructor initializing seven fields of mixed kinds, then reading them back.
// features: loop, object, new, null, array
// expect: 40646416
function Rec(name, v) {
  this.value = v || 0;
  this.list = [];
  this.owner = null;
  this.mark = 0;
  this.level = 6;
  this.fixed = true;
  this.name = name;
}
function main() {
  var sum = 0;
  for (var i = 0; i < 300000; i++) {
    var r = new Rec("r", i & 255);
    sum += r.value + r.mark + r.level + (r.fixed ? 1 : 0) + (r.owner == null ? 1 : 0) + r.list.length;
  }
  return sum;
}
main();
