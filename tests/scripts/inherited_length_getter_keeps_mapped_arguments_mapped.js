// A getter that saves `this`, reached from a mapped `arguments`
// whose own `length` was deleted: the object escapes, so it stays
// mapped to the parameters after the function returns.
// expect: 7 5

var saved;
Object.defineProperty(Object.prototype, 'length', {get: function () { saved = this; return 7; }, configurable: true});
function f(a) { delete arguments.length; var n = arguments.length; return [n, function () { a = 5; }]; }
var r = f(1);
r[1]();
delete Object.prototype.length;
r[0] + ' ' + saved[0];
