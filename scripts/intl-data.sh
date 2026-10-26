#!/usr/bin/env bash
# Generate data/intl/icu4x.postcard, the locale data `Intl` (the `intl`
# feature) reads at runtime: ICU4X's blob of what its collator needs, for
# the locales below, from CLDR and ICU's export of pinned versions.
#
#   cargo install --locked icu4x-datagen@2.4.0   # the ICU4X of Cargo.lock
#   scripts/intl-data.sh [locale …]               # default: en en-US
#
# No deduplication: each locale has its own data, so a locale has data
# if and only if the blob lists it (how `intl::icu::available` tells).
# Collation types other than the standard one are left out (datagen
# still adds the Han scripts' defaults, `und-Hans` and `und-Hant`).
# DefaultLocale (`intl::DEFAULT_LOCALE`) must be among the locales.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ $# -gt 0 ]; then locales=("$@"); else locales=(en en-US); fi
icu4x-datagen --format blob \
    --cldr-tag 49.0.0 --icuexport-tag release-79.1rc \
    --markers CollationSpecialPrimariesV1 CollationRootV1 CollationTailoringV1 CollationDiacriticsV1 \
        CollationJamoV1 CollationMetadataV1 CollationReorderingV1 NormalizerNfdDataV1 NormalizerNfdTablesV1 \
    --attribute-filter 'collator=/^$/' \
    --locales "${locales[@]}" --deduplication none \
    -o data/intl/icu4x.postcard -W
ls -l data/intl/icu4x.postcard
