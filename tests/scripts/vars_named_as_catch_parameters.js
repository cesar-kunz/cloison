// `catch (e) { var e = 1; }` declares the function's `e` and stores to the
// catch parameter, also when other block-scoped bindings rename names.
// expect: 3,true,2,true,3,4,9,function,undefined,7,undefined

let r = [];
function f() { try { throw 2 } catch (e) { var e = 3; r.push(e); } r.push(e === undefined); }
function g() { try { throw 2 } catch (e) { var e; r.push(e); } r.push(e === undefined); }
function h() { var e = 9; try { throw 2 } catch (e) { var e = 3, x = e + 1; r.push(e, x); } r.push(e); }
function k() { try { throw 2 } catch (e) { var e = function () {}; r.push(typeof e); } return typeof e; }
f(); g(); h(); r.push(k());
try { throw 5 } catch (q) { var q = 7; r.push(q); } r.push(typeof q);
r.join();
