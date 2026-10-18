# chess-coach

A Rust library for turning your chess games into opening lessons and spaced-repetition practice. **Used on [chess.player.gg](https://chess.player.gg).**

Licensed under the [MIT License](https://github.com/pragmaplatform/chess-coach/blob/main/LICENSE). The crate is independent of the application: it has no database, HTTP, account, or queue dependencies, and all Cargo dependencies are public. Stockfish runs as an external UCI process and is not bundled with this crate.

The crate provides:

- Legal SAN replay and UCI/FEN conversion through `cozy-chess`.
- Opening decision extraction for either color, capped by full move number.
- An `Engine` trait, plus an optional asynchronous Stockfish subprocess adapter.
- Candidate detection, deeper confirmation, weakness aggregation and practice prioritization.
- Legal alternative-move grading and optional deterministic FSRS review scheduling.
- Serializable data types, stable request fingerprints, and optional OpenAPI schemas.

## Getting started

Requires Rust 1.88 or newer. Add the crate to your dependencies:

```toml
[dependencies]
chess-coach = "0.4.0"
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

For the full pipeline, pass a `Game`, an `AnalysisPolicy`, and an `Engine` to `analysis::analyze_game`, then combine observations with `ranking::aggregate`. Use `practice::grade` to judge a submitted move and, with the `fsrs` feature, `review::FsrsScheduler` to compute its next review. Your application owns importing games, storing reports and exercises, and scheduling engine work. The runnable example below demonstrates analysis with Stockfish.

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

`practice::grade` accepts an evaluation for the submitted legal move. The caller obtains that evaluation through its engine/cache using the exercise's saved search profile; alternatives do not have to equal the stored best move. The application snapshots both thresholds when issuing an exercise: `correct` for at most 2 percentage points of loss, `good` for more than 2 but below the position’s cutoff, and `incorrect` at or above that cutoff. Exercises pin `opening_threshold` (a numeric grading cutoff of 10 points through full move 5), with a baseline `minimum_loss` of 5 points afterward. Follow-ups carry both thresholds and evaluate their own move number. Exercises without `opening_threshold` retain their original cutoff. Inconclusive comparisons receive `inconclusive`. Feedback contains the grade, score loss, best/played evaluations and the best continuation in SAN. Exercises saved without `minimum_loss` retain their original binary grading until completed.

Best and submitted scores must have the same `EvaluationSource`; grading rejects mixed cloud/local comparisons. The application pins cloud variations with the exercise and uses them for clear answers. Missing data and borderline or inconsistent answers fall back to local comparisons, evaluating a new local best as well when the original best came from cloud. Feedback contains the actual pair used to grade. Old evaluations without a source deserialize as Stockfish.

In version 0.3, `grade` and `grade_with_evidence` no longer take an assistance flag, and `Grade::Assisted` is removed. Historical `"assisted"` grades deserialize as `Inconclusive`, preserving their saved evaluations without assigning a new independent verdict or recall rating. They serialize as `"inconclusive"`. Existing stored review events and memory states are unchanged; do not rebuild historical recall events from the remapped grades.

Use `practice::grade_with_evidence(&exercise, submitted_uci, evidence)` without replacing the saved exercise's best evaluation or confidence. Pass `practice::Evidence::Search(comparison)` for a single raw search pair, or `Evidence::Verification { current, previous }` when a stronger search ran. Both comparisons must belong to the same position/history, submitted move, and engine identity. Cached, inline, and worker execution should use this same grading entry point. The actual evaluations used for grading are returned in feedback.

`practice::assess` exposes the resolved comparison and a typed uncertainty cause: score ordering, score drift, a mate contradiction within a pair, a changed mating outcome across searches, mismatched sources or moves, an unsupported objective, or unspecified inherited uncertainty. Retaining the evidence prevents an ordinary score-order disagreement from being mistaken for failed verification, and prevents accepting a later search that contradicts earlier mate evidence. These diagnostic types do not change the serialized exercise or feedback contracts. `grade` and `verify_comparison` remain available for existing callers; new integrations should use `grade_with_evidence`. Lesson discovery continues using the strict `MoveEvaluation::compared_with` rule.

Practice verification can recover an acceptable alternative when both evaluations have larger node counts, sources agree, mating outcomes are unchanged, and the final loss is more than one percentage point below the exercise's pass/fail cutoff. Either both losses must be safely acceptable and differ by at most two points, or the earlier loss must be within one point of the cutoff while the submitted move's evaluation stays within two points. For example, a preferred move changing from +0.41 to +0.14 while the submitted move stays at −0.14/−0.15 supports a `good` verdict at the usual five-point cutoff. The captured Caro-Kann `Qa5+` regression covers this case.

`practice::grade` also accepts an exact copy of the root search's selected best evaluation despite inherited score uncertainty, provided the exercise's objective is still supported. Matching the move name alone does not bypass disagreement between evaluations. Recovery uses `confidence: "unverified"`, not `"confirmed"`, because it establishes sufficient grading evidence without claiming absolute score convergence. Frontends should render the result from `grade`; `unverified` is not an inconclusive grade. Invalidated mating objectives remain `inconclusive`.

For an `avoid_mate` exercise whose confidence is not `inconclusive`, grading also accepts a Stockfish centipawn answer that scores above a clearly winning Stockfish reference (at least 0.90 in the existing logistic score model, roughly +6 pawns). Both lines then support escaping mate while retaining a winning position, even when their ordering exceeds the general consistency tolerance. The captured Caro-Kann `fxe5` case (+7.34 versus +6.61) receives `correct` with `unverified` confidence. Original scores and lines remain in the feedback. The same recovery applies after stronger searches only when the earlier evidence also satisfies the acceptance checks. It does not override failed verification, mate-score contradictions, cloud comparisons, or other objectives. It applies directly in `grade`, including when the exercise already uses the maximum search budget; no additional API call is required.

## Review scheduling

Enable the optional scheduler in your dependencies:

```toml
chess-coach = { version = "0.4.0", features = ["fsrs"] }
```

Enable `fsrs` to use `review::FsrsScheduler`, backed by the pinned [`fsrs` 6.6.2 release](https://github.com/open-spaced-repetition/fsrs-rs/releases/tag/v6.6.2). It uses FSRS-6 defaults, a 90% desired retention target, and a maximum interval of 36,500 days. Retention is a target to validate on your users, not a guarantee. Lower the maximum interval in `FsrsConfig` if your application needs a shorter horizon. Scheduling is deterministic and uses caller-supplied UTC epoch seconds. There is no random interval fuzz or implicit system clock.

```rust
# #[cfg(feature = "fsrs")]
# {
use chess_coach::{practice::Grade, review::{FsrsConfig, FsrsScheduler, RecallRating}};

let scheduler = FsrsScheduler::new(FsrsConfig::default())?;
let now = 1_800_000_000;
let mut state = scheduler.new_state(now);
let grade = Grade::Correct; // From practice::grade, after an independent attempt.

if let Some(rating) = RecallRating::from_grade(grade) {
    let transition = scheduler.review(&state, rating, now)?;
    // Application: atomically persist the event and state; deduplicate by event ID.
    state = transition.state;
}
let recall_probability = scheduler.retrievability(&state, now)?;
# let _ = recall_probability;
# }
# Ok::<(), chess_coach::Error>(())
```

Move quality and recall difficulty are separate. The optional `RecallRating::from_grade` helper maps both `correct` and `good` to FSRS `Good`, `incorrect` to `Again`, and `inconclusive` to `None` (leave state unchanged). If you collect subjective recall difficulty, pass `Hard`, `Good`, or `Easy` for a successful answer. `Hard` means successful recall with difficulty; an engine-perfect move is not automatically `Easy`. Teaching shown before an attempt is an exposure and should not call `review`.

`ReviewState.memory` is `Option<ReviewedMemory>`: a required `MemoryState` (stability in days and difficulty) paired with a required `last_review_at` timestamp, or `None` for unseen items. `ReviewState` also stores the due date, counters, initialization provenance, and a memory-parameter fingerprint. `ReviewTransition` includes the event, elapsed/assigned intervals, and a fingerprint of the entire configuration. Persist configurations under these identities and retain chronological events with item/event IDs, objective grades, rating provenance, and assistance/exposure information. Persist state and events atomically. Equal timestamps are allowed; event deduplication belongs to the application.

Use `scheduler.config_id()` and `scheduler.parameters_id()` for configuration identities, and `scheduler.decay()` for database recall calculations. The [storage integration guide](https://github.com/pragmaplatform/chess-coach/blob/main/docs/review-storage.md) includes the forgetting-curve contract, SQL, fixed compatibility vectors, and a runnable transactional storage example. Scheduling failures are typed as `Error::Review(ReviewError::...)`, including parameter mismatch, invalid state, and backwards timestamps; callers do not need to parse messages.

Run the scheduling and storage example from the repository root without Stockfish or a database:

```bash
cargo run --no-default-features --features fsrs --example review_storage
```

Elapsed days are `floor((now - last_review_at) / 86_400)`, including zero for same-day reviews, consistently in scheduling and replay. The first review has elapsed time zero. Intervals are rounded to the nearest whole day and clamped to `[1, maximum_interval_days]`; backwards times and timestamp overflow are errors. `Again` increments lapses even on a new item, preserving this crate's previous counter meaning. Short retries (for example, ten minutes after a failure or an hour after assistance) are application session policy, separate from the returned long-term due date. Log actual retries too.

All review data types support Serde and optional OpenAPI without enabling the scheduler. `FsrsConfig::default()` and `FsrsScheduler` require `fsrs`. New state and configuration formats identify `fsrs6_v1`; unknown versions and invalid state/configuration data are rejected. The Rust API and JSON shape intentionally replace the old `ReviewState`, `review::schedule`, `step`, `LEARNED_STEP`, and `learned()` contract. Applications that previously scheduled reviews must enable `fsrs` and migrate explicitly.

### Migrating saved progress

Read the old JSON as `LegacyReviewState`. `scheduler.migrate(&legacy, &history, seed)` preserves its due date and counters and retains an unchanged `legacy` snapshot for rollback. It does not reschedule the collection. Adopt the returned state atomically; keep subsequent events if you need to reconstruct progress after rollback.

- With complete history, supply every counted event as `ReviewEvent { at, rating }` and `None` for the seed. Counts must agree with the saved review/lapse counters. Historical correct/good grades can use the binary mapping, labeled as inferred in your event store; inconclusive events are not counted.
- With partial or no history, supply an explicit `MemorySeed { memory, last_review_at }` immediately before the known suffix. The result is labeled `estimated`. The caller owns the estimate and its supporting evidence; seed values are not synthetic optimizer reviews. A seed must stand for at least one prior review.
- With no prior reviews, empty history needs no seed. An experienced item without history or an explicit seed returns an error and leaves existing data alone. Keep it on your application's legacy path until you have evidence or choose an estimate. Step 0 cannot identify the last outcome, and saturated step 6 can correspond to 60 or 120 days, so the crate does not invent timestamps from steps.

After changing memory parameters, use `scheduler.rebuild(&state, &history, seed)` to reconstruct memory under the new configuration while preserving the current due date, counters, and legacy snapshot. Unseeded history must be complete and end at the stored last review time; partial history needs an explicit estimate. Scheduling rejects a state from a different parameter set. Changing only desired retention or the maximum interval does not require rebuilding memory and takes effect on the next review. Keep parameter optimization in the application or a separate offline tool; the crate does not run training jobs.

### Retention and chess skill

Use one memory item per user and stable position/objective/answer contract. Do not treat different examples of a theme as repetitions of one interchangeable FSRS item. Exercise variety, theme balancing, related-position spacing, and unseen-position assessment belong to the application. Stability and recall probability do not establish transferable chess mastery, so the replacement API has no `learned()` flag.

Before switching a production application, collect history and run FSRS in shadow mode alongside its existing scheduling. Evaluate prediction calibration on later independent reviews, then compare delayed recall, unseen-position performance, and total practice time in a prospective trial. Fewer reviews or a better prediction score alone do not establish better chess learning. The former scheduler can remain an application experiment control; it is not a second permanent scheduling implementation in this crate.

## Features and verification

GitHub Actions runs formatting, Clippy (with warnings treated as errors), feature-matrix tests, and an all-feature check on Rust 1.88 on all pushes. The Stockfish integration tests remain opt-in.

- `uci` (default): Tokio subprocess adapter. Custom `Engine` implementations work with or without this feature; disable default features to omit the adapter.
- `fsrs` (opt-in): FSRS-6 memory scheduler. Review data types remain available without this feature.
- `openapi`: optional schema generation with chess-prefixed type names.

```bash
cargo test --all-features
cargo test --no-default-features
cargo test --no-default-features --features fsrs
cargo +1.88.0 check --locked --all-features
STOCKFISH_PATH=/path/to/stockfish cargo test --all-features --test coaching stockfish_scores -- --ignored
```

The external-engine tests are opt-in so ordinary unit tests remain self-contained. To include the coaching-quality corpus as well, run `STOCKFISH_PATH=/path/to/stockfish cargo test --all-features -- --ignored`.

## Custom engines

Disable default features to use analysis and grading without the Tokio subprocess adapter; add `features = ["fsrs"]` if you also want scheduling. Implement `Engine::evaluate` with scores from the root position's **side-to-move perspective**, honoring `root_move` when present. `Evaluation.line` must start with `best_move` and contain legal UCI moves. Evaluate the supplied move history, not just its final FEN, so repetition and draw rules are retained. Return errors for failed searches.

Cloud evaluations are optional; the default implementation returns no cached result. If supplied, variations must be ordered best first, use `EvaluationSource::LichessCloud`, share the same root and search, and honor the requested minimum depth. The caller provides transport and caching.

## License and acknowledgements

The source, examples, and tests in this crate are available under the [MIT License](https://github.com/pragmaplatform/chess-coach/blob/main/LICENSE), copyright 2026 FirstLook. Dependencies retain their own licenses. `cozy-chess` supplies move legality and SAN/UCI conversion; `esca` supplies opening classification, using the CC0 opening catalogue from lichess-org/chess-openings. Optional `fsrs` is [BSD-3-Clause licensed](https://github.com/open-spaced-repetition/fsrs-rs/blob/v6.6.2/LICENSE); distributions using it must retain its copyright, license conditions, and disclaimer, and respect its non-endorsement condition. Stockfish is a separately installed GPL-3.0 engine; the crate does not distribute its executable or network files.

### Optional sacrifice annotations

`analysis::analyze_game` does not search for sacrifices. Hosts can run
`tactics::is_candidate` after publishing a report, then call `tactics::scan` on a
small shortlist using their own low-priority `Engine`. A scan makes at most eight
engine calls at two caller-supplied node budgets. The host owns concurrency,
foreground-work checks, caching, and the number of candidates per report.

The first detector handles missed best moves which offer a knight, bishop, rook,
or queen for a material deficit of at least two pawn units. It excludes already
capturable pieces and ordinary exchanges; the deficit must remain after the
player's next move. It verifies soundness against best defence at both budgets,
and marks `trap` only if the selected capture loses at least 0.10 on the engine
score's logistic scale relative to best defence. That scale is not an observed
human win rate. The least valuable legal attacker is preferred, with UCI order breaking ties.
No trap is marked if best defence also captures the offered piece.
Only one capture is examined; this conservative
pass can miss other sacrifices, immediate material recoveries, and traps.

Store the returned scan in `Observation.tactics`. Missing/null means not checked;
an empty `opportunities` list means checked without a verified finding. Existing
weaknesses already embed their example observation. Findings do not change
mistake eligibility, themes, or practice grading. Reply evaluations explicitly
use the position **after** the offer and the opponent's score perspective.

For a local engine smoke check and timing sample:

```sh
cargo run --example tactics -- /path/to/stockfish
```
