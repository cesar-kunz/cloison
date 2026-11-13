// `parseInt` converts its input before its radix (§19.2.5).
// expect: string,radix 12 S 597 17

var log = []; var input = { toString() { log.push('string'); return '12'; } };
var radix = { valueOf() { log.push('radix'); return 10; } };
var r = parseInt(input, radix);
var e; try { parseInt({ toString() { throw 'S'; } }, { valueOf() { log.push('never'); return 10; } }); } catch (x) { e = x; }
log.join(',') + ' ' + r + ' ' + e + ' ' + parseInt(255, 16) + ' ' + parseInt(17, 0);
