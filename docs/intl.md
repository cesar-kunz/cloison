# Intl

`Intl` (ECMA-402, the ECMAScript Internationalization API) is an
optional Cargo feature, `intl`, built on [ICU4X](https://icu4x.unicode.org),
the Unicode Consortium's Rust internationalization library. The default
build has no `Intl` and none of ICU4X's code: the feature adds the code,
and the locale data stays out of the binary, a file read at runtime.

```sh
cargo build --release --features intl
cargo test --release --features intl
```

So far it is `Intl.Collator`, and `String.prototype.localeCompare`
through it: what TypeScript's compiler uses
(its language service sorts completions with
`new Intl.Collator(locale, { usage: "sort", sensitivity: "variant", numeric: true }).compare`,
and `organizeImports` asks `Intl.Collator.supportedLocalesOf`); and
`Intl.Segmenter`, which Univer makes at startup
(`new Intl.Segmenter(undefined, { granularity: "grapheme" })`).

## What is there

- `Intl`, with its `@@toStringTag`.
- `Intl.Collator(locales, options)`, with `new` or without, subclassable:
  the locale list canonicalized, the options read in the spec's order
  (`usage`, `localeMatcher`, `collation`, `numeric`, `caseFirst`, then
  after resolving the locale `sensitivity`, `ignorePunctuation`), the
  locale's `-u-` keywords `kn` and `kf` taken unless an option overrides
  them (ResolveLocale).
- `Intl.Collator.prototype.compare` (a getter: the collator's bound
  compare function, made once), `resolvedOptions()`, `@@toStringTag`.
- `Intl.Collator.supportedLocalesOf(locales, options)`.
- `String.prototype.localeCompare(that, locales, options)` compares as
  `Intl.Collator(locales, options)` does (ECMA-402's definition of it;
  without the feature it compares code units). TiddlyWiki sorts its
  titles with it. Without locales and options it is one collator, made
  once. With a locale string and no options (`x.localeCompare(y, "en")`)
  the string's collator is kept, by the string as given, so a call
  costs about what a call without locales does (a sort of 200,000
  strings: 0.55 s, 2.66 s before; 0.46 s without locales); nothing of
  it shows, a string having nothing to read, and a malformed tag is
  not kept: it is a `RangeError` each time. Otherwise (options, or
  locales that are an object) all is read at each call, as the
  specification has it, and the collator found among those made.

- `Intl.Segmenter(locales, options)`, with `new` only, subclassable:
  `localeMatcher` read, the locale resolved, then `granularity`
  (`grapheme`, the default, `word` or `sentence`).
  `resolvedOptions()` (`locale`, `granularity`), `@@toStringTag`,
  `Intl.Segmenter.supportedLocalesOf(locales, options)`.
- `Intl.Segmenter.prototype.segment(string)`: a Segments object, with
  `containing(index)` and `@@iterator`; its iterator (of
  `%Iterator.prototype%`, tagged `Segmenter String Iterator`) with
  `next()`. The segment data objects are `{segment, index, input}`,
  with `isWordLike` after them if the granularity is `word` (true for
  a segment of letters or digits). The segments are UAX #29's extended
  grapheme clusters, words or sentences, by its default rules, the
  same in every locale; see the gaps for the words of the scripts
  written without spaces.

`sensitivity` maps to ICU4X's strength and case level, `numeric` and
`caseFirst` to its preferences, `ignorePunctuation` to shifted alternate
handling.

test262's `intl402/Collator`: 61 of 65 pass. The rest need data the
blob hasn't (Thai's and German's, the search tailorings) or
`Intl.NumberFormat`. `intl402/Segmenter`: 77 of 79 pass; the other 2
need Serbian's data. The feature adds
243 KB of code (6.36 to 6.61 MB of text), 42 KB of it the segmenters;
the default build changes by a class and an empty list.

## The data

`data/intl/icu4x.postcard` (496 KB), an ICU4X blob of what the
collator needs, for `en` and `en-US`, and of the grapheme cluster, word
and sentence break rules (10 KB the first, 31 KB the other two, of no
locale), from CLDR 49 and ICU 79.1's export (Unicode 17). It has none
of what ICU4X cuts the words of Chinese, Japanese, Thai, Lao, Khmer and
Burmese with: the Chinese and Japanese dictionary (2.0 MB), and for the
other four scripts dictionaries (1.0 MB) or LSTM models (0.3 MB).
`scripts/intl-data.sh [locale …]` makes it with `icu4x-datagen` (the
ICU4X version `Cargo.lock` has).

It is read on the first `Intl.Collator` or `Intl.Segmenter` made or
`localeCompare` call (not at startup), once per
process: `CLOISON_INTL_DATA`'s file, else the repository's (the path the
build was made from). Without it, making a collator or a segmenter
throws an `Error` saying where it looked.

The blob has no fallback between locales (`--deduplication none`), so a
locale is available if and only if the blob has it: that is how
`supportedLocalesOf` and locale resolution tell (the segmenter's too:
its available locales are the collator's, the rules being of none). BestAvailableLocale
strips subtags until one is (`en-GB` is served by `en`); a locale with
none falls back to DefaultLocale, `en-US`. More locales are a longer
argument list to the script (each tailoring adds its own data; the
root collation, most of the blob, is shared).

## Code

- `src/intl/icu.rs` is the boundary to ICU4X: loading the data, locale
  tags canonicalized, whether a locale has data, collators made from a
  key (a locale and options) and compared over UTF-16, the segmenter
  of each granularity and the first boundary it finds in UTF-16. Plain
  values in and out, nothing of the VM's.
- `src/intl/mod.rs` is the JS side ECMA-402 shares between
  constructors: CanonicalizeLocaleList, the options (GetOption,
  GetOptionsObject, CoerceOptionsToObject), BestAvailableLocale,
  ResolveLocale, LookupSupportedLocales.
- `src/intl/collator.rs` is `Intl.Collator`.
- `src/intl/segmenter.rs` is `Intl.Segmenter`, its Segments objects and
  their iterators.

An `Intl` object is of class `Intl` (`layout::Class::Intl`), its internal
slots hidden in-object slots beyond its shape's capacity, as a native
closure's data. The first says which kind of object it is (a collator,
a segmenter, a Segments object, a Segment Iterator), what a method
checks its receiver by. A collator's others are its ICU4X collator (an
index into `vm.intl`'s, which interns them by locale and options), its
bound `compare`, its locale string and its other resolved options packed
in an int; a segmenter's are its locale string and its granularity, a
Segments object's its string and the granularity, an iterator's its string, the granularity and where its
next segment starts (the VM has one ICU4X segmenter of each granularity
for them all, made with the first `Intl.Segmenter` of it). An iterator
finds each segment from the end of the one before, and `containing`
from the last line feed before the index: no rule of UAX #29 looks back past a boundary, so that
is the segment a pass over the whole string finds (a unit test of
`icu.rs` checks it on random strings). The prototypes are
per realm (`vm.intl_roots`).

Host builds link statically, and Cargo applies `.cargo/config.toml`'s
target flags to proc macros too unless told otherwise; ICU4X has proc
macros, so the config sets `target-applies-to-host = false` (a nightly
Cargo setting): build scripts and proc macros are built without the
target's flags. The default build's binary is the same either way.

## Gaps

Beyond the other constructors (`NumberFormat`, `DateTimeFormat`,
`PluralRules`, `Locale`, …) and `Intl.getCanonicalLocales`:

- `Intl.Segmenter`'s words are without dictionaries. In the scripts
  written without spaces between words (Chinese and Japanese: Han
  characters and hiragana; Thai, Lao, Khmer, Burmese) a run of one
  script's letters is one segment, word-like, where V8 (ICU's
  dictionaries) cuts words: `今日はいい天気ですね` is one segment (V8:
  `今日|は|いい|天気|です|ね`), `สวัสดีครับ` one (V8: `สวัสดี|ครับ`),
  `Hello世界` is `Hello|世界` as in V8, and `カタカナとひらがな` is
  `カタカナ|とひらがな` (V8: `カタカナ|と|ひ|ら|が|な`). Nothing throws;
  punctuation, spaces and other scripts still cut (`中。文` is three
  segments), katakana and Korean go by the rules as in V8, and
  graphemes and sentences aren't concerned (`今日は晴れ。明日は雨。` is
  two sentences). That data would add 2.3 MB to the blob, or 3.0 MB.
- The segmenter's rules are UAX #29's defaults, without CLDR's
  tailorings, of the root or of a locale. Known differences: `a:b` is
  one word (the colon is between letters as an apostrophe is; ICU 74
  and SpiderMonkey cut it in three, JavaScriptCore's ICU doesn't); a
  lone `_` is word-like (as in SpiderMonkey; not in V8); and Greek's
  question mark `;` doesn't end a sentence for `el` (ICU4X has that
  one tailoring; the segmenter isn't given the locale, which the blob
  has no data of anyway).
- Locale data for English only: `de`, `fr` and the rest resolve to
  `en-US` (their tailorings aren't in the blob). For the segmenter too,
  though its rules are of no locale: the specification lets an
  implementation choose its available locales, and test262 wants `sr`
  (`fr`, `de`, `zh-CN` too) among them and `xyz` not, which takes a
  list of the locales there are; the blob has none but those it has
  data of, and saying yes to any well-formed tag would fail other
  tests.
- Canonicalization is syntactic (case, order of variants and keywords,
  `true` values dropped); no alias data, so a deprecated tag (`iw`) stays
  as it is.
- `usage: "search"` collates as `sort` does (the blob has no search
  tailorings), and `collation` resolves to `"default"` whatever was
  asked (no other collation types in the data).
- The locale-dependent defaults are English's: `ignorePunctuation`
  false, `sensitivity` `variant` for search too.
- DefaultLocale is `en-US`, not the environment's (`LANG`).
- `best fit` matching is `lookup`.
- `toLocaleString`, `toLocaleUpperCase` and the like don't go through
  `Intl` (ECMA-402 says they should): they behave as without it.

## A plugin, later

The feature puts ICU4X's code in the binary. It could instead live in a
shared library loaded at runtime, so even an `intl` build carries only
the JS side (tens of KB), and ICU4X and its data come with the plugin:

- A crate in this repository, `cloison-intl`, built as a `cdylib`,
  holding `src/intl/icu.rs` and ICU4X (with its data compiled in, or
  still read from a blob).
- Its interface the boundary `icu.rs` already is, as `extern "C"`
  functions: `intl_canonicalize(tag, len, out, cap) -> len`,
  `intl_available(locale, len) -> i32`,
  `intl_collator_new(key) -> handle`,
  `intl_collator_compare(handle, a, a_len, b, b_len) -> i32`,
  `intl_collator_free(handle)`, and `intl_abi_version() -> u32`, which
  the engine checks before using any (Rust has no stable ABI: only C
  types cross; errors as codes, with a function for the last message).
- The engine `dlopen`s it (`libc` has the calls) on the first `Intl`
  use: `CLOISON_INTL_PLUGIN`, else next to the executable. Without it,
  `Intl` is absent (or throws), as without the feature.
- Host builds would have to link dynamically: `dlopen` from a static
  glibc binary is unsupported in practice (the plugin would need the
  same glibc), and static musl has no `dlopen`. That costs the startup
  time static linking saves (see "Building" in [cloison in depth](README-long.md#building)).

Every call crosses the C interface, but `Intl`'s calls are coarse (one
comparison or formatting each); string arguments go as UTF-16 pointers
and lengths, without copies.
