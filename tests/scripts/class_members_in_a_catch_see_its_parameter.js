// A class's field initializers, static blocks, computed keys and methods
// in a catch clause see its parameter (14.15.3), as other functions do.
// (A class is a block-scoped declaration: the script's catch parameters
// are renamed apart anyway.)
// expect: 1 2 3 4 5

var log = [];
function a() { try { throw 1; } catch (e) { class C { x = e; } return new C().x; } }
function b() { try { throw 2; } catch (e) { class C { static x = e; } return C.x; } }
function c() { var r; try { throw 3; } catch (e) { class C { static { r = e; } } } return r; }
function d() { try { throw "k"; } catch (e) { class C { [e] = 4; } return new C().k; } }
function f() { var e = 0; try { throw 5; } catch (e) { class C { m() { return e; } } return new C().m(); } }
log.push(a(), b(), c(), d(), f());
log.join(" ");
