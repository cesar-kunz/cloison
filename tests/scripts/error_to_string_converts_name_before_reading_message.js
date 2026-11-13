// expect: E: msg n,N,m

var log = [];
var o = { get name(){ log.push("n"); return {toString(){ log.push("N"); return "E"; }}; },
          get message(){ log.push("m"); return "msg"; } };
Error.prototype.toString.call(o) + " " + log.join(",");
