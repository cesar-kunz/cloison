// An object used as a dictionary: computed string keys stored, looked up and counted.
// features: loop, string, object, array, call, for-in
// expect: 1026000
function countWords(words, dict) {
  for (var i = 0; i < words.length; i++) {
    var w = words[i];
    dict[w] = (dict[w] || 0) + 1;
  }
}
function main() {
  var words = [];
  for (var i = 0; i < 200; i++) words.push("w" + ((i * 17) % 23));
  var dict = {};
  for (var r = 0; r < 2000; r++) countWords(words, dict);
  var s = 0;
  for (var k in dict) s += dict[k] * k.length;
  return s;
}
main();
