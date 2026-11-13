// A numeric literal as a property key is its `String` (`1e21` "1e+21",
// `2e-7` "2e-7"), in literals, classes and patterns.
// expect: ["16","1e+21","2e-7","0.5","1.5e+300","0.1","5e-324"] constructor,1e+21 13

var o = { 1e21: 1, 2e-7: 2, 0.5: 3, 1.5e300: 4, 0x10: 5, .1: 7, 5e-324: 8 };
class C { 1e21() {} }
var { 1e21: a, 0.5: b } = o;
JSON.stringify(Object.keys(o)) + " " + Object.getOwnPropertyNames(C.prototype).join() + " " + a + b;
