// Math functions the suites call beyond DeltaBlue's: several arguments to
// min/max, and the trigonometry.
// for: crypto, raytrace, box2d, mandreel, zlib, navier-stokes
// expect: 5,-2,-Infinity,Infinity,NaN,-3,-2,-2,3,0,1.4142135623730951,1.4142135623730951,0.8414709848078965,0.5403023058681398,1.5574077246549023,0.7853981633974483,2.356194490192345,0.5235987755982989,1.0471975511965979,2.718281828459045,2147483645,-35
var r = [Math.max(1, 5, 3), Math.min(4, -2, 9), Math.max(), Math.min(), Math.max(1, NaN),
         Math.floor(-2.5), Math.ceil(-2.5), Math.round(-2.5), Math.round(2.5), Math.abs(-0),
         Math.sqrt(2), Math.pow(2, 0.5), Math.sin(1), Math.cos(1), Math.tan(1),
         Math.atan(1), Math.atan2(1, -1), Math.asin(0.5), Math.acos(0.5), Math.exp(1),
         Math.imul(0x7fffffff, 3), Math.imul(-5, 7)];
r.join(",");
