# Changelog

## 0.1.1

### Changed

- `OpeningThreshold::default()` now excludes all non-mating practice candidates through full move 5, regardless of evaluation loss or material themes. Conclusive missed-mate and allowed-mate lessons remain eligible; normal loss-based selection resumes on move 6.
- Added `OpeningThreshold.mates_only`. Set it to `false` for the previous numeric-cutoff behavior. Older serialized thresholds without this field retain that behavior; existing Rust struct literals must supply the field or use `..Default::default()`.
- Updated the standalone example to policy `openingcoach-logistic-v5`. Numeric search and grading thresholds are unchanged. Applications must regenerate saved analysis and practice cards to apply the new exclusion.
- Included this changelog in the published crate.

### Tests

- Covered large early losses for both colors, the move-5 boundary, the `1.d4 c5 2.Bf4` material-loss example, legacy policy serialization, cloud/local verification, and retained mating lessons.

## 0.1.0

Initial standalone release of `chess-coach`, an MIT-licensed Rust library for opening analysis and spaced-repetition practice. Requires Rust 1.88 or newer.

### Added

- Legal SAN replay, UCI/FEN conversion, opening classification, and opening decision extraction for either color.
- An `Engine` trait for custom engines and an optional asynchronous Stockfish UCI adapter with search budgets, process timeouts, and full move history.
- Opening mistake analysis with confirmation and verification searches, configurable practice thresholds, and distinct centipawn and mate evaluations.
- Optional cloud evaluation support through the engine interface, with local confirmation for borderline or inconsistent comparisons.
- Transposition-aware weakness aggregation, frequency-weighted practice priorities, and teaching themes.
- Practice grading for legal alternative moves, SAN continuations, follow-up positions, and deterministic spaced-repetition scheduling.
- Serializable analysis, practice, and review data, stable engine request fingerprints, and optional OpenAPI schemas.
- A standalone Stockfish analysis example and an opt-in coaching-quality test corpus.
