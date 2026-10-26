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

So far it is `Intl.Collator` alone, what TypeScript's compiler uses
(its language service sorts completions with
`new Intl.Collator(locale, { usage: "sort", sensitivity: "variant", numeric: true }).compare`,
and `organizeImports` asks `Intl.Collator.supportedLocalesOf`).

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

`sensitivity` maps to ICU4X's strength and case level, `numeric` and
`caseFirst` to its preferences, `ignorePunctuation` to shifted alternate
handling.

test262's `intl402/Collator`: 61 of 65 pass. The rest need data the
blob hasn't (Thai's and German's, the search tailorings) or
`Intl.NumberFormat`. The feature adds 200 KB of code (5.76 to 5.96 MB
of text); the default build changes by a class and an empty list.

## The data

`data/intl/icu4x.postcard` (454 KB), an ICU4X blob of what the
collator needs, for `en` and `en-US`, from CLDR 49 and ICU 79.1's
export (Unicode 17, the version of cloison's other tables).
`scripts/intl-data.sh [locale …]` makes it with `icu4x-datagen` (the
ICU4X version `Cargo.lock` has).

It is read on the first `Intl.Collator` made (not at startup), once per
process: `CLOISON_INTL_DATA`'s file, else the repository's (the path the
build was made from). Without it, making a collator throws an `Error`
saying where it looked.

The blob has no fallback between locales (`--deduplication none`), so a
locale is available if and only if the blob has it: that is how
`supportedLocalesOf` and locale resolution tell. BestAvailableLocale
strips subtags until one is (`en-GB` is served by `en`); a locale with
none falls back to DefaultLocale, `en-US`. More locales are a longer
argument list to the script (each tailoring adds its own data; the
root collation, most of the blob, is shared).

## Code

- `src/intl/icu.rs` is the boundary to ICU4X: loading the data, locale
  tags canonicalized, whether a locale has data, collators made from a
  key (a locale and options) and compared over UTF-16. Plain values in
  and out, nothing of the VM's.
- `src/intl/mod.rs` is the JS side ECMA-402 shares between
  constructors: CanonicalizeLocaleList, the options (GetOption,
  CoerceOptionsToObject), BestAvailableLocale, ResolveLocale,
  LookupSupportedLocales.
- `src/intl/collator.rs` is `Intl.Collator`.

An `Intl` object is of class `Intl` (`layout::Class::Intl`), its internal
slots hidden in-object slots beyond its shape's capacity, as a native
closure's data: a collator's are its ICU4X collator (an index into
`vm.intl`'s, which interns them by locale and options), its bound
`compare`, its locale string and its other resolved options packed in an
int. The prototypes are per realm (`vm.intl_roots`).

Host builds link statically, and Cargo applies `.cargo/config.toml`'s
target flags to proc macros too unless told otherwise; ICU4X has proc
macros, so the config sets `target-applies-to-host = false` (a nightly
Cargo setting): build scripts and proc macros are built without the
target's flags. The default build's binary is the same either way.

## Gaps

Beyond the other constructors (`NumberFormat`, `DateTimeFormat`,
`PluralRules`, `Segmenter`, `Locale`, …) and `Intl.getCanonicalLocales`:

- Locale data for English only: `de`, `fr` and the rest resolve to
  `en-US` (their tailorings aren't in the blob).
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
- `String.prototype.localeCompare`, `toLocaleString` and the like don't
  go through `Intl` (ECMA-402 says they should): they behave as without
  it.

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
  time static linking saves (see the README's "Building").

Every call crosses the C interface, but `Intl`'s calls are coarse (one
comparison or formatting each); string arguments go as UTF-16 pointers
and lengths, without copies.
