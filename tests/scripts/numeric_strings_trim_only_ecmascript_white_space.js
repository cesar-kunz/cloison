// StringToNumber and StringToBigInt trim ECMAScript's white space and
// line terminators: not U+0085, but U+FEFF.
// expect: NaN,1,1,SyntaxError,2

var r = [String(Number('\u00851\u0085')), String(Number('﻿1　')), String(Number(' 1\t'))];
try { r.push(BigInt('\u00851\u0085')); } catch (e) { r.push(e.name); }
r.push(BigInt('﻿2 '));
r.join();
