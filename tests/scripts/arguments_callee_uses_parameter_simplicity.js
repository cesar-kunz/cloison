// expect: ok

function check(ok, message) {
    if (!ok) throw new Error(message);
}
function throwsTypeError(f) {
    try { f(); } catch (e) { return e instanceof TypeError; }
    return false;
}

function strictArgs(a) { "use strict"; return arguments; }
var thrower = Object.getOwnPropertyDescriptor(strictArgs(), "callee").get;

function poisoned(a) {
    var d = Object.getOwnPropertyDescriptor(a, "callee");
    check(d !== undefined, "missing callee");
    check(!("value" in d) && !("writable" in d), "data descriptor");
    check(d.get === thrower && d.set === thrower, "wrong thrower");
    check(d.enumerable === false && d.configurable === false, "attributes");
    check(throwsTypeError(function () { return a.callee; }), "read");
    check(throwsTypeError(function () { a.callee = 1; }), "sloppy write");
    check(throwsTypeError(function () {
        Object.defineProperty(a, "callee", { value: 1 });
    }), "redefinition");
    check(delete a.callee === false, "deletion");
}

function defaults(a = 1) { return arguments; }
function rest(...xs) { return arguments; }
function objectPattern({ x }) { return arguments; }
function arrayPattern([x]) { return arguments; }
function emptyPattern({}) { return arguments; }
function inDefault(a = arguments) { return a; }
function withEval(a = 1) { return eval("arguments"); }
function* generator(a = 1) { yield arguments; }

poisoned(strictArgs());
poisoned(defaults());
poisoned(defaults(2));
poisoned(rest(1, 2));
poisoned(objectPattern({ x: 1 }));
poisoned(arrayPattern([1]));
poisoned(emptyPattern({}));
poisoned(inDefault());
poisoned(withEval());
poisoned(generator().next().value);
poisoned(Function("a = 1", "return arguments")());
poisoned(eval("(function (a = 1) { return arguments; })")());

function plain(a) { return arguments; }
function empty() { return arguments; }
function dataCallee(f) {
    var a = f();
    var d = Object.getOwnPropertyDescriptor(a, "callee");
    check(d.value === f && !("get" in d), "simple callee");
    check(d.writable && d.configurable && !d.enumerable, "simple attributes");
}
dataCallee(plain);
dataCallee(empty);

function unmapped(a = 0) {
    arguments[0] = 9;
    check(a === 1, "unexpected parameter mapping");
    a = 7;
    check(arguments[0] === 9, "unexpected reverse mapping");
}
unmapped(1);

function hot(a = 1) {
    var sum = 0;
    for (var i = 0; i < 5; i++) sum += i;
    return arguments;
}
for (var i = 0; i < 2000; i++) poisoned(hot(i));
"ok";
