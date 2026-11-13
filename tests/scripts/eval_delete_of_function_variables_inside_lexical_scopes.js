// A direct eval's `delete` of a binding its scope object inherits
// deletes it where it is: the function's `var` isn't deleted, false
// (cloison-fuzz BUGS.md #16); a `with` object's inherited property is
// still true.
// expect: false,number,false trueundefined true false

var out = [];
{
  let z = 1;
  out.push((function (p) { var l = 3; return [eval("delete l"), typeof l, eval("delete p")].join(); })(5));
  out.push((function () { eval("var q = 1"); return eval("delete q") + typeof q; })());
  out.push((function () { var o = Object.create({ x: 1 }); var r; with (o) { r = eval("delete x"); } return r; })());
  out.push((function () { let b = 2; return eval("delete b"); })());
}
out.join(' ');
