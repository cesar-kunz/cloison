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

use icu_collator::options::{AlternateHandling, CaseLevel, CollatorOptions, Strength};
use icu_collator::preferences::{CollationCaseFirst, CollationNumericOrdering};
use icu_collator::CollatorPreferences;
use icu_locale_core::Locale;
use icu_provider_blob::BlobDataProvider;

const DEFAULT_DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/intl/icu4x.postcard");

/// The data, read once per process (the blob is leaked: the payloads
/// borrow it for good).
fn provider() -> Result<&'static BlobDataProvider, String> {
    static DATA: OnceLock<Result<BlobDataProvider, String>> = OnceLock::new();
    DATA.get_or_init(|| {
        let path = std::env::var("CLOISON_INTL_DATA").unwrap_or_else(|_| DEFAULT_DATA.into());
        let bytes = std::fs::read(&path).map_err(|e| format!("Intl's locale data ({path}): {e} (set CLOISON_INTL_DATA)"))?;
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
        prefs.numeric_ordering = Some(if key.numeric { CollationNumericOrdering::True } else { CollationNumericOrdering::False });
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
        icu_collator::Collator::try_new_with_buffer_provider(provider()?, prefs, options).map(Collator).map_err(|e| format!("{}: {e}", key.locale))
    }

    pub(crate) fn compare(&self, a: &[u16], b: &[u16]) -> Ordering {
        self.0.as_borrowed().compare_utf16(a, b)
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
