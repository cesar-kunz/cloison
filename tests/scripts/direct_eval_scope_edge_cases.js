// A direct `eval`'s scope object leaves the function's own scope alone
// when nothing else is in sight (its `var`s deletable there), and a
// binding takes back its value, not a `var` the `eval` declared by its
// name.
// expect: ,undefined,,1,true,false

var r = [];
var initial = null, postDeletion;
(function () { eval('initial = x; delete x; postDeletion = function () { return typeof x; }; var x;'); })();
r.push(initial, postDeletion());
function testAssignment() {
  var x = 0;
  var innerX = (function () { x = (eval("var x;"), 1); return x; })();
  r.push(innerX, x);
}
testAssignment();
function F() { return eval("new.target === F"); }
r.push(new F() instanceof F, F());
r.join();
