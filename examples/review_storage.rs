//! A dependency-free model of a storage adapter for one user's one memory item.
//! Run: cargo run --no-default-features --features fsrs --example review_storage
//! Production code must use a real DB transaction, row lock, and unique event IDs.
use std::collections::BTreeMap;

use chess_coach::{
    Error,
    review::{
        FsrsConfig, FsrsScheduler, LegacyReviewState, MemoryState, RecallRating, ReviewError, ReviewEvent,
        ReviewOrigin, ReviewState, ReviewTransition, ReviewedMemory,
    },
};

#[derive(Clone)]
struct MemoryRow {
    config_id: String,
    stability_days: Option<f32>,
    difficulty: Option<f32>,
    last_review_at: Option<i64>,
    due_at: i64,
    reviews: u32,
    lapses: u32,
    origin: ReviewOrigin,
    legacy: Option<LegacyReviewState>,
    last_event_id: Option<u64>,
}

impl MemoryRow {
    fn restore(&self, scheduler: &FsrsScheduler) -> Result<ReviewState, Error> {
        let memory = match (self.stability_days, self.difficulty, self.last_review_at) {
            (Some(stability_days), Some(difficulty), Some(last_review_at)) => Some(ReviewedMemory {
                memory: MemoryState {
                    stability_days,
                    difficulty,
                },
                last_review_at,
            }),
            (None, None, None) => None,
            _ => return Err(ReviewError::InvalidState("partially stored memory").into()),
        };
        let state = ReviewState {
            version: scheduler.config().version,
            parameters_id: scheduler.parameters_id().to_owned(),
            memory,
            due_at: self.due_at,
            reviews: self.reviews,
            lapses: self.lapses,
            origin: self.origin,
            legacy: self.legacy.clone(),
        };
        state.validate()?;
        Ok(state)
    }

    fn from_state(state: &ReviewState, config_id: String, last_event_id: Option<u64>) -> Self {
        Self {
            config_id,
            stability_days: state.memory.map(|reviewed| reviewed.memory.stability_days),
            difficulty: state.memory.map(|reviewed| reviewed.memory.difficulty),
            last_review_at: state.memory.map(|reviewed| reviewed.last_review_at),
            due_at: state.due_at,
            reviews: state.reviews,
            lapses: state.lapses,
            origin: state.origin,
            legacy: state.legacy.clone(),
            last_event_id,
        }
    }
}

#[derive(Clone)]
struct StoredEvent {
    previous_event_id: Option<u64>,
    transition: ReviewTransition,
}

#[derive(Clone)]
struct Database {
    // Immutable shared records: other memory items can refer to the same ID.
    configurations: BTreeMap<String, FsrsConfig>,
    memory: MemoryRow,
    events: BTreeMap<u64, StoredEvent>,
}

impl Database {
    fn new(scheduler: &FsrsScheduler, now: i64) -> Self {
        Self {
            configurations: [(scheduler.config_id().to_owned(), scheduler.config().clone())].into(),
            memory: MemoryRow::from_state(&scheduler.new_state(now), scheduler.config_id().to_owned(), None),
            events: BTreeMap::new(),
        }
    }

    // Copy-on-write illustrates commit/rollback, not a production DB implementation.
    fn transaction(&mut self, write: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        let mut pending = self.clone();
        write(&mut pending)?;
        *self = pending;
        Ok(())
    }

    fn review(&mut self, event_id: u64, event: ReviewEvent) -> Result<(), Error> {
        self.transaction(|tx| {
            // In SQL, lock the item's memory row before checking for a duplicate.
            if let Some(existing) = tx.events.get(&event_id) {
                if existing.transition.event == event {
                    return Ok(());
                }
                return Err(ReviewError::InvalidHistory("event ID reused with different input").into());
            }
            let config = tx
                .configurations
                .get(&tx.memory.config_id)
                .ok_or(ReviewError::InvalidState("missing configuration"))?;
            let scheduler = FsrsScheduler::new(config.clone())?;
            let previous = tx.memory.restore(&scheduler)?;
            let transition = scheduler.review(&previous, event.rating, event.at)?;
            let next = MemoryRow::from_state(&transition.state, transition.config_id.clone(), Some(event_id));
            tx.events.insert(
                event_id,
                StoredEvent {
                    previous_event_id: tx.memory.last_event_id,
                    transition,
                },
            );
            tx.memory = next;
            // Persist engine feedback and rating provenance in this same transaction.
            Ok(())
        })
    }

    fn history(&self) -> Vec<ReviewEvent> {
        let mut history = Vec::new();
        let mut event_id = self.memory.last_event_id;
        while let Some(id) = event_id {
            let event = &self.events[&id];
            history.push(event.transition.event);
            event_id = event.previous_event_id;
        }
        history.reverse();
        history
    }
}

fn main() -> Result<(), Error> {
    let scheduler = FsrsScheduler::new(FsrsConfig::default())?;
    let mut db = Database::new(&scheduler, 1_800_000_000);
    let first = ReviewEvent {
        at: 1_800_000_000,
        rating: RecallRating::Good,
    };
    db.review(20, first)?;
    db.review(20, first)?; // Duplicate delivery does not count twice.
    db.review(
        10,
        ReviewEvent {
            at: first.at + 86_400,
            rating: RecallRating::Again,
        },
    )?;
    assert_eq!(db.events.len(), 2);
    assert_eq!(db.configurations.len(), 1);

    // Event IDs need not encode order. Follow the selected predecessor chain.
    let state = db.memory.restore(&scheduler)?;
    let replayed = scheduler.replay(&db.history(), None)?.unwrap();
    assert_eq!(Some(replayed.memory), state.memory.map(|reviewed| reviewed.memory));
    assert_eq!(
        Some(replayed.last_review_at),
        state.memory.map(|reviewed| reviewed.last_review_at)
    );

    // A failure even after a provisional write rolls back the entire transaction.
    let result = db.transaction(|tx| {
        tx.events.clear();
        Err(ReviewError::InvalidState("simulated feedback write failure").into())
    });
    assert!(result.is_err());
    assert_eq!(db.events.len(), 2);
    assert_eq!(db.memory.restore(&scheduler)?, state);
    println!("Stored and replayed two reviews with one shared configuration; duplicate and rollback checks passed.");
    Ok(())
}

#[test]
fn storage_round_trip() {
    main().unwrap();
}

#[test]
fn storage_requires_all_or_none_of_the_memory_columns() {
    let scheduler = FsrsScheduler::new(FsrsConfig::default()).unwrap();
    for mask in 0..8 {
        let mut row = MemoryRow::from_state(&scheduler.new_state(100), scheduler.config_id().to_owned(), None);
        row.stability_days = (mask & 1 != 0).then_some(10.0);
        row.difficulty = (mask & 2 != 0).then_some(5.0);
        row.last_review_at = (mask & 4 != 0).then_some(0);
        row.reviews = u32::from(mask != 0);
        assert_eq!(row.restore(&scheduler).is_ok(), mask == 0 || mask == 7);
    }
}
