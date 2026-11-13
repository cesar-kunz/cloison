// `Error.prototype.stack`: an accessor whose getter gives an error's
// kind and message, and whose setter makes the receiver's own `stack`.
// expect: function,get stack,set stack,false,true,TypeError: bad,false,,custom,true,TypeError,TypeError,TypeError

var r = [];
var d = Object.getOwnPropertyDescriptor(Error.prototype, "stack");
r.push(typeof d.get, d.get.name, d.set.name, d.enumerable, d.configurable);
var e = new TypeError("bad");
r.push(e.stack, Object.prototype.hasOwnProperty.call(e, "stack"), d.get.call({}));
e.stack = "custom";
r.push(e.stack, Object.getOwnPropertyDescriptor(e, "stack").enumerable);
try { d.set.call(e, 1); } catch (x) { r.push(x.name); }
try { Error.prototype.stack = ""; } catch (x) { r.push(x.name); }
try { d.get.call(1); } catch (x) { r.push(x.name); }
r.join();
