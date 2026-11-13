// A conversion's exception stops the operation: no element read, no
// argument converted, no comparator called after it.
// expect: throw,caught throw,caught t,caught cmp,caught v,caught

var out = [];
var log = []; var x = { toString() { log.push('throw'); throw 123; } }; var a = [x, 0];
Object.defineProperty(a, 1, { get() { log.push('next'); return 1; } });
try { a.join(); } catch (e) { log.push('caught'); } out.push(log.join(','));
log = []; var n = new Number(1); n.toString = function () { log.push('throw'); throw 1; };
var rep = [n, 0]; Object.defineProperty(rep, 1, { get() { log.push('next'); return 'b'; } });
try { JSON.stringify({}, rep); } catch (e) { log.push('caught'); } out.push(log.join(','));
log = []; var b = [{ toString() { log.push('t'); throw 2; } }, 1, 2, 3];
try { b.sort(); } catch (e) { log.push('caught'); } out.push(log.join(','));
log = [];
try { [1, 2, 3].sort(function () { log.push('cmp'); return { valueOf() { throw 3; } }; }); } catch (e) { log.push('caught'); }
out.push(log.join(','));
log = []; var c = [{ valueOf() { log.push('v'); throw 4; } }];
Object.defineProperty(c, 1, { get() { log.push('next'); return 1; } });
try { c.map(Math.abs); } catch (e) { log.push('caught'); } out.push(log.join(','));
out.join(' ');
