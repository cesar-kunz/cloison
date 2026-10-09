// `instanceof` through prototype chains, with builtins and primitives.
// for: earley-boyer, box2d, pdfjs, gbemu
// expect: true,true,true,false,true,true,false,false,true,false
function A() {}
function B() {}
B.prototype = new A();
var b = new B();
[b instanceof B, b instanceof A, b instanceof Object, new A() instanceof B, [] instanceof Array,
 [] instanceof Object, 3 instanceof Object, null instanceof Object, A instanceof Function,
 ({}) instanceof Function].join(",");
