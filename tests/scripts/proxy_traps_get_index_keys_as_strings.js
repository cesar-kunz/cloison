// A Proxy trap gets an index key as a property key, its string, not a
// number (cloison-fuzz BUGS.md #11).
// expect: get:string,set:string,getOwnPropertyDescriptor:string,defineProperty:string,has:string,getOwnPropertyDescriptor:string,defineProperty:string,deleteProperty:string

var seen = [];
var h = {};
["get", "set", "has", "deleteProperty", "defineProperty", "getOwnPropertyDescriptor"].forEach(function (n) {
  h[n] = function (t, k) { seen.push(n + ":" + typeof k); return Reflect[n].apply(null, arguments); };
});
var p = new Proxy({}, h);
p[1]; p[1] = 2; 1 in p; Object.getOwnPropertyDescriptor(p, 1); Object.defineProperty(p, 3, { value: 1, configurable: true }); delete p[1];
seen.join();
