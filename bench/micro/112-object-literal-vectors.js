// Vector math on short-lived object literals {x, y, z} (Raytrace's style).
// features: loop, double, object, call, literal
// expect: 1312497
function vadd(a, b) { return { x: a.x + b.x, y: a.y + b.y, z: a.z + b.z }; }
function vscale(a, k) { return { x: a.x * k, y: a.y * k, z: a.z * k }; }
function vdot(a, b) { return a.x * b.x + a.y * b.y + a.z * b.z; }
function main() {
  var p = { x: 0, y: 0, z: 0 }, d = { x: 0.5, y: -0.25, z: 1 }, s = 0;
  for (var i = 0; i < 500000; i++) {
    p = vadd(vscale(p, 0.5), d);
    s += vdot(p, d);
  }
  return Math.round(s);
}
main();
