// Private names written with escapes or non-ASCII letters: the same
// names as written plainly (`#\u{6F}` is `#o`); an escape that isn't an
// identifier character is a SyntaxError.
// expect: 1,2,3,4,5,s,true | SyntaxError | SyntaxError

var r = [];
class C {
  #\u{6F} = 1; #℘ = 2; #ZW_‌_NJ = 3; #$ = 4; #yield = 5;
  static #\u{6F}m() { return "s"; }
  get() { return [this.#o, this.#℘, this.#ZW_‌_NJ, this.#$, this.#yield, C.#om(), #\u{6F} in this].join(); }
}
r.push(new C().get());
try { eval("class D { #\\u0030x }"); r.push("no"); } catch (e) { r.push(e.name); }
try { eval("class D { #a\\u0020 }"); r.push("no"); } catch (e) { r.push(e.name); }
r.join(" | ");
