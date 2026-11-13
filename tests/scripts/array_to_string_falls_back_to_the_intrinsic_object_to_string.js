// expect: [object Array]

var a = [1,2]; a.join = 1;
Object.prototype.toString = () => "X";
String(a);
