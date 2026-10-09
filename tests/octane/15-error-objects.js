// Error objects: message, name, toString, instanceof, and the TypeError a
// failed access throws.
// for: navier-stokes (checksum failed), gbemu, pdfjs, earley-boyer
// expect: boom,Error,Error: boom,true,true,true,TypeError,bad,true
var e = new Error("boom");
var r = [e.message, e.name, String(e), e instanceof Error];
try { null.x; } catch (t) { r.push(t instanceof TypeError, t instanceof Error, t.name); }
try { throw new TypeError("bad"); } catch (t) { r.push(t.message, t instanceof Error); }
r.join(",");
