//! The boundary to ICU4X: all `Intl` asks of it, by plain values (locale
//! tags as strings, options as enums, UTF-16 slices) and opaque handles.
//! Nothing here sees the VM, so this module could be a plugin loaded at
//! runtime behind a C ABI (see docs/intl.md, "A plugin").
//!
//! The locale data is a postcard blob of ICU4X's (made by
//! `scripts/intl-data.sh`) read on first use: `CLOISON_INTL_DATA`'s file,
//! else the repository's `data/intl/icu4x.postcard`. Its locales have no
//! fallback between them (`--deduplication none`): a locale has data if
//! and only if the blob lists it, which [`available`] asks.

use std::cmp::Ordering;
use std::sync::OnceLock;

use icu_collator::CollatorPreferences;
use icu_collator::options::{AlternateHandling, CaseLevel, CollatorOptions, Strength};
use icu_collator::preferences::{CollationCaseFirst, CollationNumericOrdering};
use icu_locale_core::Locale;
use icu_provider_blob::BlobDataProvider;

const DEFAULT_DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/intl/icu4x.postcard");

/// The data, read once per process (the blob is leaked: the payloads
/// borrow it for good).
fn provider() -> Result<&'static BlobDataProvider, String> {
    static DATA: OnceLock<Result<BlobDataProvider, String>> = OnceLock::new();
    DATA.get_or_init(|| {
        let path = std::env::var("CLOISON_INTL_DATA").unwrap_or_else(|_| DEFAULT_DATA.into());
        let bytes =
            std::fs::read(&path).map_err(|e| format!("Intl's locale data ({path}): {e} (set CLOISON_INTL_DATA)"))?;
        let blob: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        BlobDataProvider::try_new_from_static_blob(blob).map_err(|e| format!("Intl's locale data ({path}): {e}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// `tag` canonicalized (UTS #35's canonical syntax: cases, the order of
/// variants and extension keywords, `true` values dropped), if it is a
/// well-formed Unicode BCP 47 locale identifier. (No alias data: a
/// deprecated subtag stays.)
pub(crate) fn canonicalize(tag: &str) -> Option<String> {
    Locale::try_from_str(tag).ok().map(|l| l.to_string())
}

/// ECMA-402's `sensitivity`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Sensitivity {
    Base,
    Accent,
    Case,
    Variant,
}

/// ECMA-402's `caseFirst`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum CaseFirst {
    False,
    Lower,
    Upper,
}

/// What a collator is made of: a locale with data (no extensions) and
/// options.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct CollatorKey {
    pub(crate) locale: String,
    pub(crate) sensitivity: Sensitivity,
    pub(crate) ignore_punctuation: bool,
    pub(crate) numeric: bool,
    pub(crate) case_first: CaseFirst,
}

/// A collator.
pub(crate) struct Collator(icu_collator::Collator);

impl Collator {
    pub(crate) fn new(key: &CollatorKey) -> Result<Collator, String> {
        let locale = Locale::try_from_str(&key.locale).map_err(|e| format!("{}: {e}", key.locale))?;
        let mut prefs = CollatorPreferences::from(&locale);
        prefs.numeric_ordering =
            Some(if key.numeric { CollationNumericOrdering::True } else { CollationNumericOrdering::False });
        prefs.case_first = Some(match key.case_first {
            CaseFirst::False => CollationCaseFirst::False,
            CaseFirst::Lower => CollationCaseFirst::Lower,
            CaseFirst::Upper => CollationCaseFirst::Upper,
        });
        let mut options = CollatorOptions::default();
        // (ECMA-402's sensitivities as ICU4X's docs map them.)
        options.strength = Some(match key.sensitivity {
            Sensitivity::Base | Sensitivity::Case => Strength::Primary,
            Sensitivity::Accent => Strength::Secondary,
            Sensitivity::Variant => Strength::Tertiary,
        });
        if key.sensitivity == Sensitivity::Case {
            options.case_level = Some(CaseLevel::On);
        }
        if key.ignore_punctuation {
            options.alternate_handling = Some(AlternateHandling::Shifted);
        }
        icu_collator::Collator::try_new_with_buffer_provider(provider()?, prefs, options)
            .map(Collator)
            .map_err(|e| format!("{}: {e}", key.locale))
    }

    pub(crate) fn compare(&self, a: &[u16], b: &[u16]) -> Ordering {
        self.0.as_borrowed().compare_utf16(a, b)
    }
}

/// What a segmenter cuts its text into.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Granularity {
    Grapheme,
    Word,
    Sentence,
}

/// A segmenter of one granularity, by UAX #29's rules untailored (the
/// same for every locale): extended grapheme clusters, words, sentences.
/// The words are without dictionaries: a run of Chinese or Japanese,
/// Thai, Lao, Khmer or Burmese letters is one word.
pub(crate) enum Breaker {
    Grapheme(icu_segmenter::GraphemeClusterSegmenter),
    Word(Box<icu_segmenter::WordSegmenter>),
    Sentence(icu_segmenter::SentenceSegmenter),
}

impl Breaker {
    pub(crate) fn new(granularity: Granularity) -> Result<Breaker, String> {
        use icu_segmenter::{GraphemeClusterSegmenter, SentenceSegmenter, WordSegmenter};
        let provider = provider()?;
        match granularity {
            Granularity::Grapheme => {
                GraphemeClusterSegmenter::try_new_with_buffer_provider(provider).map(Breaker::Grapheme)
            }
            Granularity::Word => {
                WordSegmenter::try_new_for_non_complex_scripts_with_buffer_provider(provider, Default::default())
                    .map(|s| Breaker::Word(Box::new(s)))
            }
            Granularity::Sentence => {
                SentenceSegmenter::try_new_with_buffer_provider(provider, Default::default()).map(Breaker::Sentence)
            }
        }
        .map_err(|e| format!("segmentation: {e}"))
    }

    /// Where the segment at the start of `units` (not empty) ends, and
    /// whether it is word-like (a word segmenter's: of letters or digits).
    pub(crate) fn first(&self, units: &[u16]) -> (usize, bool) {
        let end = units.len();
        match self {
            Breaker::Grapheme(s) => (s.as_borrowed().segment_utf16(units).find(|&b| b > 0).unwrap_or(end), false),
            Breaker::Word(s) => {
                let mut breaks = s.as_borrowed().segment_utf16(units).iter_with_word_type();
                breaks.find(|&(b, _)| b > 0).map_or((end, false), |(b, t)| (b, t.is_word_like()))
            }
            Breaker::Sentence(s) => (s.as_borrowed().segment_utf16(units).find(|&b| b > 0).unwrap_or(end), false),
        }
    }
}

/// Whether the data has collation for `locale` (a canonical tag without
/// extensions); an error if there is no data at all.
pub(crate) fn available(locale: &str) -> Result<bool, String> {
    provider()?;
    let key = CollatorKey {
        locale: locale.into(),
        sensitivity: Sensitivity::Variant,
        ignore_punctuation: false,
        numeric: false,
        case_first: CaseFirst::False,
    };
    Ok(Collator::new(&key).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the Segments objects and their iterators rely on: the
    /// segment found from a boundary is the one a pass over the whole
    /// string finds there, word-like as there but where that pass keeps
    /// an earlier segment's type (see below), of each granularity, in
    /// strings of what the
    /// rules look around (marks, joiners, flags, quotes, terminators,
    /// line ends, lone surrogates, a complex script's letters).
    #[test]
    fn a_segment_from_a_boundary_is_the_whole_strings() {
        let alphabet: Vec<Vec<u16>> = [
            "a",
            "B",
            "1",
            " ",
            ".",
            ",",
            "'",
            ":",
            "_",
            "-",
            "\n",
            "\r",
            "\"",
            "(",
            ")",
            "?",
            "!",
            ";",
            "\u{300}",
            "\u{200d}",
            "\u{ad}",
            "\u{1F600}",
            "\u{1F1EB}",
            "\u{5d0}",
            "\u{30ab}",
            "\u{3000}",
            "\u{2029}",
            "\u{85}",
            "\u{e9}",
            "\u{200b}",
            "\u{e01}",
            "\u{4e2d}",
            "\u{1100}",
            "\u{1161}",
            "\u{915}",
            "\u{94d}",
        ]
        .iter()
        .map(|s| s.encode_utf16().collect())
        .chain([vec![0xD800], vec![0xDC00]])
        .collect();
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut random = |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize % n
        };
        for granularity in [Granularity::Grapheme, Granularity::Word, Granularity::Sentence] {
            let breaker = Breaker::new(granularity).unwrap();
            for _ in 0..20000 {
                let units: Vec<u16> = (0..=random(40)).flat_map(|_| alphabet[random(alphabet.len())].clone()).collect();
                // (Each boundary with whether the segment ending there
                // is word-like; the first, at 0, ends none.)
                let whole: Vec<(usize, bool)> = match &breaker {
                    Breaker::Grapheme(s) => s.as_borrowed().segment_utf16(&units).map(|b| (b, false)).collect(),
                    Breaker::Word(s) => {
                        let breaks = s.as_borrowed().segment_utf16(&units).iter_with_word_type();
                        breaks.map(|(b, t)| (b, b > 0 && t.is_word_like())).collect()
                    }
                    Breaker::Sentence(s) => s.as_borrowed().segment_utf16(&units).map(|b| (b, false)).collect(),
                };
                let mut chained = vec![(0, false)];
                let mut at = 0;
                while at < units.len() {
                    let (n, word_like) = breaker.first(&units[at..]);
                    at += n;
                    // The whole pass keeps the type of the segment
                    // before for one whose rule sets none (an emoji
                    // after a word, marks alone): word-like there, not
                    // from its own boundary, as it is not in ICU.
                    let (_, there) = whole[chained.len()];
                    let kept = there && !word_like && whole[chained.len() - 1].1;
                    chained.push((at, word_like || kept));
                }
                assert_eq!(chained, whole, "{units:x?}");
            }
        }
    }
}
