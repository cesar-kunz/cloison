// Method call site seeing four receiver classes, each with its own implementation.
// features: loop, call, object, new, proto, array
// expect: 1750000
function K1() { this.x = 1; }  K1.prototype.f = function () { return this.x; };
function K2() { this.y = 2; }  K2.prototype.f = function () { return this.y * 2; };
function K3() { this.z = 3; }  K3.prototype.f = function () { return this.z - 1; };
function K4() { this.w = 4; }  K4.prototype.f = function () { return 0; };
function main() {
  var objs = [];
  for (var i = 0; i < 100; i++) {
    var k = i % 4;
    objs.push(k == 0 ? new K1() : k == 1 ? new K2() : k == 2 ? new K3() : new K4());
  }
  var sum = 0;
  for (var r = 0; r < 10000; r++) for (var i = 0; i < 100; i++) sum += objs[i].f();
  return sum;
}
main();
