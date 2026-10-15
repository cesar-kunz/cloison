// instanceof against classes of a three-level hierarchy.
// features: loop, object, new, proto, array, call, instanceof
// expect: 3300000
function Shape() {}
function Circle() {}
Circle.prototype = new Shape();
function Ring() {}
Ring.prototype = new Circle();
function Square() {}
Square.prototype = new Shape();
function classify(o) {
  if (o instanceof Ring) return 3;
  if (o instanceof Circle) return 2;
  if (o instanceof Square) return 5;
  return o instanceof Shape ? 1 : 0;
}
function main() {
  var os = [new Shape(), new Circle(), new Ring(), new Square(), {}];
  var s = 0;
  for (var r = 0; r < 300000; r++)
    for (var i = 0; i < os.length; i++) s += classify(os[i]);
  return s;
}
main();
