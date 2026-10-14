# Persisting FSRS reviews

Run the storage example without an engine or database:

```sh
cargo run --no-default-features --features fsrs --example review_storage
```

[`examples/review_storage.rs`](../examples/review_storage.rs) demonstrates typed memory
columns, one shared immutable configuration, event deduplication, atomic writes,
and ordered replay. Its copy-on-write store illustrates commit/rollback for one
user/item; use actual database transactions and locks in production.

## Configuration and state identities

Call `scheduler.config_id()` to key a shared configuration record. Store
`scheduler.config()` as JSON, `scheduler.parameters_id()` as its memory-parameter
identity, and optionally `scheduler.decay()` as a numeric column for SQL queries.
Insert configurations once; never change the contents under an existing ID.

The full configuration ID includes version, parameters, desired retention, and
maximum interval. The parameter ID includes only version and parameters. Changing
retention or the interval limit changes the former but not the latter. Identities
are SHA-256 fingerprints of the versioned Rust types' JSON encoding. Treat them as
opaque keys: obtain them from these methods rather than hashing JSONB text, whose
key ordering and numeric rendering can differ. Compatibility fixtures pin their
values. Changes to that encoding require explicit version/compatibility review.

Store the memory item's configuration ID alongside typed stability, difficulty,
last-review time, due time, counters, origin, and optional legacy snapshot. Restore
the state using the configuration actually associated with that memory. Validate
the reconstructed `ReviewState`; reject partially populated memory fields. Do not
attach a new parameter ID to existing memory: use `rebuild` after parameter changes.
Preserve origin and legacy metadata rather than assuming every item started fresh.

`ReviewState.memory` is `Option<ReviewedMemory>`. Unseen items serialize as
`"memory": null`. Reviewed items serialize as:

```json
"memory": {
  "memory": { "stability_days": 14.0, "difficulty": 5.0 },
  "last_review_at": 1800000000
}
```

There is no top-level `last_review_at`. The nested estimate and timestamp are both
required. Flat database columns can stay flat: restore `Some(ReviewedMemory)` only
when stability, difficulty, and timestamp are all present, and `None` only when
all three are absent. Reject partial rows; call `ReviewState::validate()` after
restoration to check bounds, counters, and due-time consistency.

This shape replaces the earlier uncommitted 0.2 prototype. If an application has
already stored prototype JSON, explicitly wrap its existing memory and timestamp
together, leaving due dates, counters, identities, origin, and legacy snapshots
unchanged. Convert both-null values to `memory: null`; reject partial pairs. Apply
this conversion to stored transition states too. Configuration JSON and IDs are
unchanged. Fixed-interval records still use `LegacyReviewState` and `migrate`.

## Atomic review writes and replay

Within one transaction:

1. Lock the user's memory item and check the unique event ID. An identical
   redelivery is a no-op; reusing an ID for different input is an error.
2. Load its immutable configuration, restore memory, and call `review`.
3. Insert the event and transition, linking to the previous event if using chains.
   Store objective grade and rating provenance separately; a grade proxy is not a
   subjective recall rating. Guided teaching exposures do not become recall events.
4. Update current memory, its configuration ID, and latest-event pointer; persist
   related feedback before committing. Any failure rolls the whole transaction back.

Store a stable sequence or predecessor chain: timestamps alone cannot order equal-time
events, and event IDs need not be chronological. Replay only the selected item's
history. If an account merge chooses one schedule, retain its chain rather than
concatenating unrelated branches. `replay` reconstructs memory under the scheduler's
parameters; it does not reproduce historical due-date decisions across configuration
changes. For auditing those decisions, retain each transition and its configuration.
Partial history needs an explicit `MemorySeed`; never fabricate earlier events.

## Database recall contract: `fsrs6_v1`

For reviewed memory with stability `s` and the stored configuration's positive
`decay = scheduler.decay()`:

```text
t = floor((as_of_epoch_seconds - last_review_epoch_seconds) / 86400)
recall = (1 + t / s * (0.9 ^ (-1 / decay) - 1)) ^ (-decay)
```

Elapsed time uses whole 86,400-second periods, not UTC dates. Recall is 1 within
the first elapsed day, and 0.9 when elapsed days equal stability. The constant 0.9
defines stability; it is not replaced by the configured desired retention. Use the
item's persisted decay, not the newest default. Unseen items have no estimate (`NULL`).
The Rust API rejects backwards timestamps and elapsed-day overflow. Validate those
conditions before scheduling; a display query can return `NULL` for future reviews.

Example PostgreSQL query (illustrative table names, `$1` is an explicit as-of time):

```sql
SELECT CASE
    WHEN m.stability_days IS NULL OR m.last_review_at IS NULL
         OR m.last_review_at > $1::timestamptz THEN NULL
    ELSE POWER(
        1.0 + FLOOR(EXTRACT(EPOCH FROM ($1::timestamptz - m.last_review_at)) / 86400.0)
          / m.stability_days::double precision
          * (POWER(0.9::double precision, -1.0 / c.decay::double precision) - 1.0),
        -c.decay::double precision
    ) END AS recall
FROM review_memory m
JOIN review_configs c ON c.id = m.config_id;
```

Use a consistent as-of time across ordering and aggregation. PostgreSQL double
precision and the scheduler's f32 math need tolerance, not bitwise equality. The
[fixed vectors](../tests/fixtures/review/fsrs6_v1.json) include two configurations,
all ratings, equal-time events, elapsed-day boundaries, and stability extremes.
Recall comparisons use an absolute tolerance of `1e-6`. Scheduling fixtures compare
intervals, timestamps, counters, and IDs exactly, and memory floats with small tolerances.

## Compatibility checks and error handling

`tests/review_contract.rs` reads the fixed JSON without regenerating it. It tests
serialization with no scheduler feature, configuration identities, future scheduling
from stored state, and the database recall formula. Regenerate only as an intentional
contract change, reviewing the diff and version implications:

```sh
cargo run --quiet --no-default-features --features fsrs --example review_vectors \
  > tests/fixtures/review/fsrs6_v1.json
```

Match `Error::Review(ReviewError::ParametersMismatch)` to request a rebuild;
`InvalidState` to surface corrupt/inconsistent memory; `NonChronologicalTime` to
reject out-of-order input; and `TimestampOverflow`, `InvalidHistory`,
`InvalidConfiguration`, or `Backend` as appropriate. Match variants, not display text.
Serde parsing errors remain Serde errors; use typed `validate()` for programmatically
restored rows. None of these errors authorizes silently resetting a player's progress.
