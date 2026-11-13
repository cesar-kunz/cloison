// A derived class's constructor checks what it returns as it returns,
// after its `finally` blocks and outside its own `try`s: a `catch` in it
// doesn't catch the TypeError, a `super()` in a `finally` counts.
// expect: TypeError object TypeError TypeError ReferenceError

var r = [];
function t(f) { try { var o = f(); return typeof o; } catch (e) { return e.name; } }
class B {}
r.push(t(() => new (class extends B { constructor() { super(); try { return 0; } catch (e) { return; } } })()));
r.push(t(() => new (class extends B { constructor() { try { return; } finally { super(); } } })()));
r.push(t(() => new (class extends B { constructor() { super(); try { return 1; } finally { } } })()));
r.push(t(() => new (class extends B { constructor() { try { super(); return 7; } catch (e) { return {}; } } })()));
r.push(t(() => new (class extends B { constructor() { return; } })()));
r.join(" ");
