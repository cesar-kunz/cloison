// An arrow in a derived class's constructor may call `eval` before
// `super()`: only its code's use of `this` (or of `super.x`, or an
// arrow's in it) is a ReferenceError then, not the call.
// expect: pre:2 this:ReferenceError super:ReferenceError inner:ReferenceError post:2:8 nt:function

var log = [];
class A { constructor() { this.a = 1; } get x() { return 7 } }
class B extends A {
  constructor() {
    var f = () => eval('1 + 1');
    log.push("pre:" + f());
    try { (() => eval('this'))(); } catch (e) { log.push("this:" + e.name); }
    try { (() => eval('super.x'))(); } catch (e) { log.push("super:" + e.name); }
    try { (() => eval('(() => this)()'))(); } catch (e) { log.push("inner:" + e.name); }
    super();
    log.push("post:" + f() + ":" + (() => eval('this.a + super.x'))());
  }
}
new B();
class C extends A { constructor() { var g = () => () => eval('typeof new.target'); log.push("nt:" + g()()); super(); } }
new C();
log.join(" ");
