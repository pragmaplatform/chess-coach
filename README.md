# chess-coach

A Rust library for turning your chess games into opening lessons and spaced-repetition practice. **Used on [chess.player.gg](https://chess.player.gg).**

Licensed under the [MIT License](LICENSE). The crate is independent of the application: it has no database, HTTP, account, or queue dependencies, and all Cargo dependencies are public. Stockfish runs as an external UCI process and is not bundled with this crate.

The crate provides:

- Legal SAN replay and UCI/FEN conversion through `cozy-chess`.
- Opening decision extraction for either color, capped by full move number.
- An `Engine` trait, plus an optional asynchronous Stockfish subprocess adapter.
- Candidate detection, deeper confirmation, weakness aggregation and practice prioritization.
- Legal alternative-move grading and deterministic review scheduling.
- Serializable data types, stable request fingerprints, and optional OpenAPI schemas.

## Getting started

Requires Rust 1.88 or newer. Add the crate to your dependencies:

```toml
[dependencies]
chess-coach = "0.1.1"
```

Replay a game and extract the player's opening decisions without running an engine:

```rust
use chess_coach::game::{self, Color, Game};

let game = Game {
    id: "example".into(),
    player_color: Color::White,
    moves_san: "e4 e5 Nf3 Nc6 Bb5".into(),
};
let decisions = game::decisions(&game, 15)?;
assert_eq!(decisions.len(), 3);
assert_eq!(decisions[0].played_uci, "e2e4");
# Ok::<(), chess_coach::Error>(())
```

For the full pipeline, pass a `Game`, an `AnalysisPolicy`, and an `Engine` to `analysis::analyze_game`, then combine observations with `ranking::aggregate`. Use `practice::grade` to judge a submitted move and `review::schedule` to compute its next review. Your application owns importing games, storing reports and exercises, and scheduling engine work. The runnable example below demonstrates analysis with Stockfish.

The public API is pre-1.0. Persist the policy and engine profile alongside results; changing search budgets or thresholds can change the lessons generated.

## Standalone example

Install Stockfish 19, then create `game.json`:

```json
{"id":"example","player_color":"white","moves_san":"f3 e5 g4 Qh4#"}
```

From the repository root:

```bash
cargo run --example analyze -- /path/to/stockfish game.json
```

The example prints a JSON report and elapsed time; no database or credentials are required. Input is a whitespace-separated, canonical SAN move list, such as the `moves` field in a Lichess NDJSON export. PGN headers, comments, variations, arbitrary starting positions, and chess variants are not accepted. The entire move list is validated, although only the selected player's opening decisions are analyzed.

## Analysis contract

`AnalysisPolicy` records the cutoff, thresholds, engine identity and search budgets. The example uses policy `openingcoach-logistic-v5`: 15 full moves, a 100,000-node initial search, a 1,000,000-node confirmation search and a selective 5,000,000-node verification search. The default `opening_threshold` excludes all non-mating lessons through full move 5. Its numeric loss cutoff remains 10 points in that window for search escalation and grading; the baseline is 5 points afterward. Candidates at half the position’s numeric loss threshold (5 points early, then 2.5), score inversions, and differences in mating outcomes advance to confirmation. Comparisons within one percentage point of the numeric loss threshold (9–11 points early, then 4–6 inclusive), score inversions, mating differences, and unstable comparisons advance to verification. Clear mistakes keep the existing confirmation budget. Both moves are searched again when a stage is needed, and the final evaluations determine the loss and practice eligibility. The application and example default to three engine threads. This is a bounded search heuristic, not exhaustive error detection or a guarantee that scores have converged.

**Only conclusive missed-mate and allowed-mate lessons qualify through full move 5** with `OpeningThreshold::default()`. Non-mating lessons are excluded regardless of evaluation loss or material themes. From move 6 onward, conclusive losses of at least **5 percentage points** qualify; supported mating lessons remain eligible below that cutoff. Set `mates_only: false` to use the numeric early-opening cutoff instead. Older serialized thresholds without `mates_only` deserialize as `false`, and policies without `opening_threshold` retain their original cutoff. Saved observations and practice cards are not retroactively filtered; applications must regenerate them under the new policy to apply the exclusion. `severity` distinguishes `inaccuracy` (5–<10 points), `mistake` (10–<15) and `blunder` (15+); losses below 5 have no severity. The existing `Observation.mistake` boolean indicates practice eligibility through the position’s loss threshold or a supported mating lesson. A weakness's severity uses its average loss; each occurrence also has its own severity. Frequency-weighted ranking is unchanged.

Older serialized policies without `verification_nodes` retain their two-stage search. Older observations and weaknesses remain readable with a null severity until a new analysis refresh. Verification must use a larger node budget than confirmation when enabled. Engine cache keys already include the budget, so cached searches at lower budgets cannot stand in for verification searches.

`Engine::cloud_evaluations` is an optional extension, enabled by `AnalysisPolicy.cloud_min_depth`. The default implementation and standalone example remain local. The application supplies Lichess cloud variations at depth 22 or higher. A clear comparison can skip local search only when both moves occur in the same response; missing or shallow data uses local analysis, while borderline or inconsistent cloud comparisons go directly to local confirmation. Cloud positions must have a zero halfmove clock so earlier repetition/draw history cannot be lost. Saved observations retain the cloud variations and expose the evaluation source; their `profile` is the local fallback profile when cloud was used. Cloud transport, caching and throttling belong to the application service, not this crate.

For each decision, both scores come from the same root position and the same player's perspective. The played move is evaluated with `searchmoves`; evaluating a child position and forgetting to invert its score would be incorrect. Mate scores remain distinct from centipawn scores. The UCI adapter validates the engine name, applies thread/hash limits, resets search state, preserves the full move history, and kills timed-out processes. Failed searches are errors rather than fabricated evaluations.

Teaching positions group by piece placement, side to move, castling rights and legal en-passant availability. Halfmove/fullmove counters and move order are excluded from that grouping, so transpositions combine. Exact engine cache requests retain the complete history and full FEN, engine name, settings, node budget, timeout and optional root move. Two transpositions can share a weakness while keeping separate evaluations for repetition and draw rules.

A weakness is keyed by teaching position and played move. Each game contributes its earliest occurrence once. `reached_games` also includes games where the position was played correctly. `priority` is the sum of probability losses divided by the successfully analyzed game count; it is the ranking value, not an Elo reward.

Move loss is calculated from the engine evaluations:

```text
P(cp) = 1 / (1 + exp(-0.00368 * cp))
loss = max(0, P(best) - P(played))
```

Winning/losing mate scores map to 1/0. These losses rank practice opportunities; they do not predict rating improvement. Reports contain analyzed-game counts, weaknesses and themes, with no potential Elo gain or modeled recovery scenario. One-off mistakes remain eligible for practice. Previously serialized reports remain readable; obsolete gain fields are ignored.

## Practice contract

`practice::grade` accepts an evaluation for the submitted legal move. The caller obtains that evaluation through its engine/cache using the exercise's saved search profile; alternatives do not have to equal the stored best move. The application snapshots both thresholds when issuing an exercise: `correct` for at most 2 percentage points of loss, `good` for more than 2 but below the position’s cutoff, and `incorrect` at or above that cutoff. Exercises pin `opening_threshold` (a numeric grading cutoff of 10 points through full move 5), with a baseline `minimum_loss` of 5 points afterward. Follow-ups carry both thresholds and evaluate their own move number. Exercises without `opening_threshold` retain their original cutoff. Inconclusive comparisons receive `inconclusive`; otherwise assisted answers receive `assisted`. Feedback contains the grade, score loss, best/played evaluations and the best continuation in SAN. Exercises saved without `minimum_loss` retain their original binary grading until completed.

Best and submitted scores must have the same `EvaluationSource`; grading rejects mixed cloud/local comparisons. The application pins cloud variations with the exercise and uses them for clear answers. Missing data and borderline or inconsistent answers fall back to local comparisons, evaluating a new local best as well when the original best came from cloud. Feedback contains the actual pair used to grade. Old evaluations without a source deserialize as Stockfish.

The caller persists `ReviewState` and supplies UTC epoch seconds to `review::schedule`. Correct and good reviews advance through 1, 3, 7, 14, 30, 60 and 120 days without adding a lapse. Incorrect answers return in 10 minutes; assisted answers in one hour. Both reset progression.

## Features and verification

GitHub Actions runs formatting, Clippy (with warnings treated as errors), all-feature tests, and a build without default features on all pushes. The Stockfish integration tests remain opt-in.

- `uci` (default): Tokio subprocess adapter. Disable default features to provide your own `Engine`.
- `openapi`: optional schema generation with chess-prefixed type names.

```bash
cargo test --all-features
cargo check --no-default-features
STOCKFISH_PATH=/path/to/stockfish cargo test --all-features --test coaching stockfish_scores -- --ignored
```

The external-engine tests are opt-in so ordinary unit tests remain self-contained. To include the coaching-quality corpus as well, run `STOCKFISH_PATH=/path/to/stockfish cargo test --all-features -- --ignored`.

## Custom engines

Disable default features to use the analysis, grading, and scheduling modules without the Tokio subprocess adapter. Implement `Engine::evaluate` with scores from the root position's **side-to-move perspective**, honoring `root_move` when present. `Evaluation.line` must start with `best_move` and contain legal UCI moves. Evaluate the supplied move history, not just its final FEN, so repetition and draw rules are retained. Return errors for failed searches.

Cloud evaluations are optional; the default implementation returns no cached result. If supplied, variations must be ordered best first, use `EvaluationSource::LichessCloud`, share the same root and search, and honor the requested minimum depth. The caller provides transport and caching.

## License and acknowledgements

The source, examples, and tests in this crate are available under the [MIT License](LICENSE), copyright 2026 FirstLook. Dependencies retain their own licenses. `cozy-chess` supplies move legality and SAN/UCI conversion; `esca` supplies opening classification, using the CC0 opening catalogue from lichess-org/chess-openings. Stockfish is a separately installed GPL-3.0 engine; the crate does not distribute its executable or network files.
