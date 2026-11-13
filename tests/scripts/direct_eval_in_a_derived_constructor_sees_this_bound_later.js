// A direct `eval`'s code in a derived class's constructor (or an arrow
// in one, or an `eval` in that code) reads the constructor's `this`
// binding, not its value at the call: its arrows made before `super()`
// see `this` once `super()` binds it (a ReferenceError called before),
// as do `super.x`, by an arrow's `super()` too; a second `super()` is
// a ReferenceError; field initializers have run. With a scope object
// (`// eval`) or the site's bindings, 3000 times. A function's `this`
// in such code is its own. SpiderMonkey's, JSC's and V8's values.
// expect: true true true true true ReferenceError ReferenceError7 true ReferenceError 2 2 true ReferenceError 8 3000 3000 true true

var log = [];
function t(f) { try { log.push(String(f())); } catch (e) { log.push(e.name); } }
class Base { constructor() { this.b = 1; } }
Base.prototype.p = 7;
t(() => { class D extends Base { constructor() { const g = (() => eval("() => this"))(); super(); this.ok = g() === this; } } return new D().ok; });
t(() => { class D extends Base { constructor() { const g = eval("() => this"); super(); this.ok = g() === this; } } return new D().ok; });
t(() => { class D extends Base { constructor() { const g = eval("() => () => this"); super(); this.ok = g()() === this; } } return new D().ok; });
t(() => { class D extends Base { constructor() { const g = eval("() => eval('this')"); super(); this.ok = g() === this; } } return new D().ok; });
t(() => { class D extends Base { constructor() { const g = eval("eval('() => this')"); super(); this.ok = g() === this; } } return new D().ok; });
t(() => { class D extends Base { constructor() { const g = eval("() => this"); g(); super(); } } return new D(); });
t(() => { class D extends Base { constructor() { const g = eval("() => super.p"); try { g(); } catch (e) { var r = e.name; } super(); this.v = r + g(); } } return new D().v; });
t(() => { class D extends Base { constructor() { const s = () => super(); const g = eval("() => this"); s(); this.ok = g() === this; } } return new D().ok; });
t(() => { class D extends Base { constructor() { super(); super(); } } return new D(); });
t(() => { class D extends Base { f = this.b + 1; constructor() { const g = eval("() => this.f"); super(); this.v = g(); } } return new D().v; });
t(() => { class D extends Base { constructor() { let x = 1; const g = eval("() => this.b + x // eval"); super(); this.v = g(); } } return new D().v; });
with ({}) t(() => { class D extends Base { constructor() { const g = eval("() => this"); super(); this.ok = g() === this; } } return new D().ok; });
t(() => { class D extends Base { constructor() { const g = eval("this"); super(); } } return new D(); });
t(() => { class D extends Base { constructor() { super(); this.v = eval("this.b + super.p"); } } return new D().v; });
t(() => { var n = 0; class D extends Base { constructor() { const g = eval("() => this"); super(); if (g() === this) n++; } } for (let i = 0; i < 3000; i++) new D(); return n; });
t(() => { var n = 0; class D extends Base { constructor() { const g = eval("() => this // eval"); super(); if (g() === this) n++; } } for (let i = 0; i < 3000; i++) new D(); return n; });
t(() => { class D extends Base { constructor() { const g = eval("() => new.target"); super(); this.v = g() === D; } } return new D().v; });
t(() => { class D extends Base { constructor() { const g = eval("(function () { return this; })"); super(); this.v = g.call(5) == 5; } } return new D().v; });
log.join(" ");
