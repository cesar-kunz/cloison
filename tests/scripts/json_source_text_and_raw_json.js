// `JSON.parse`'s reviver gets a primitive's source text while the value
// is still the parsed one; `JSON.rawJSON` text is what `stringify`
// writes.
// expect: a=-0e1 0=12345678901234567890 1=string b=object =object | {"n":1e1000,"s":["y"]} | true | false | true |  | SyntaxError | SyntaxError

var seen = [];
JSON.parse('{"a": 1.50, "b": [12345678901234567890, "x\\u0041"], "a": -0e1}', function (k, v, c) {
  seen.push(k + "=" + (c.source === undefined ? typeof v : c.source));
  if (k === "0") this[1] = "changed";
  return v;
});
var raw = JSON.rawJSON("1e1000");
var r = [seen.join(" "), JSON.stringify({ n: raw, s: [JSON.rawJSON('"y"')] }), JSON.isRawJSON(raw), JSON.isRawJSON({}),
  Object.isFrozen(raw), Object.getPrototypeOf(raw)];
try { JSON.rawJSON(" 1"); } catch (e) { r.push(e.name); }
try { JSON.rawJSON("{}"); } catch (e) { r.push(e.name); }
r.join(" | ");
