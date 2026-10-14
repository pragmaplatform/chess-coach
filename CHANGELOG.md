# Changelog

## 0.2.1

### Fixed

- Accept the root search's selected best evaluation during practice even when absolute score drift made the comparison inconclusive, while retaining mating-objective checks and assisted grading.
- Add `practice::verify_comparison` for stronger practice searches. Recover safely acceptable alternatives and borderline earlier verdicts when supported by stable loss or stable submitted-move evidence; keep strict analysis verification unchanged. Applications must adopt this helper for alternative-move recovery.
- Cover the captured Caro-Kann `Qa5+` case, score drift, grading cutoffs, contradictory comparisons, search budgets, sources, and mating safeguards with regression tests.

## 0.2.0

### Breaking

- Replaced the fixed review scheduler with optional FSRS-6 scheduling (`fsrs` feature). Use `FsrsScheduler::review` with a separate `RecallRating`; `review::schedule`, progression steps, and `learned()` were removed.
- Replaced persisted `ReviewState` with a versioned memory state. Its optional `ReviewedMemory` groups the memory estimate and last-review timestamp so they cannot be independently absent. Read old records as `LegacyReviewState` and explicitly migrate using full history or a caller-supplied estimate. Migration preserves due dates, counters, and the original snapshot.
- Review validation and scheduling failures now use `Error::Review(ReviewError)` instead of generic configuration strings. Serde decoding keeps its native error type.

### Added

- Deterministic scheduling and recall estimates using pinned `fsrs` 6.6.2, default memory parameters, and a 90% retention target. Rust 1.88 remains supported.
- Grade-to-recall proxy mapping, chronological history replay, and parameter-change reconstruction. Inconclusive grades produce no review rating; short retries and optimization remain application concerns.
- Serializable, validated state/configuration contracts and OpenAPI schemas. Data types can be used without the scheduler dependency.
- Tests for upstream parity, elapsed-day handling, migration, configuration changes, and persistence; feature-matrix and MSRV CI checks.
- Public configuration/parameter identity and named decay accessors, with a documented database recall contract and SQL example.
- Fixed JSON compatibility fixtures for stored configurations, states, events, identities, scheduling, and recall boundaries; an explicit fixture generator.
- A runnable storage adapter example covering shared configurations, typed restoration, transaction rollback, duplicate events, and ordered replay.

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
