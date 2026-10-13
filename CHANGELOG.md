# Changelog

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
