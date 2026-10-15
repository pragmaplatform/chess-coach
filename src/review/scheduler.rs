use super::{
    FsrsConfig, LegacyReviewState, MemorySeed, MemoryState, RecallRating, ReviewError, ReviewEvent, ReviewOrigin,
    ReviewState, ReviewTransition, ReviewVersion, ReviewedMemory,
};
use crate::{Error, fingerprint};

const DAY: i64 = 86_400;

impl Default for FsrsConfig {
    fn default() -> Self {
        Self {
            version: ReviewVersion::Fsrs6V1,
            parameters: fsrs::DEFAULT_PARAMETERS.to_vec(),
            desired_retention: 0.9,
            maximum_interval_days: 36_500,
        }
    }
}

/// Pure, deterministic FSRS-6 scheduling. Reuse one instance per configuration.
/// No clock, storage, random interval fuzz, session queue, or optimization jobs.
pub struct FsrsScheduler {
    model: fsrs::FSRS,
    config: FsrsConfig,
    parameters_id: String,
    config_id: String,
}

impl FsrsScheduler {
    pub fn new(config: FsrsConfig) -> Result<Self, Error> {
        config.validate()?;
        Ok(Self {
            model: fsrs::FSRS::new(&config.parameters).map_err(backend_error)?,
            parameters_id: fingerprint(&(config.version, &config.parameters))?,
            config_id: fingerprint(&config)?,
            config,
        })
    }

    pub fn config(&self) -> &FsrsConfig {
        &self.config
    }

    /// Stable fingerprint of the full configuration, including retention and interval limits.
    /// Persist the configuration once under this key, before inserting states/events using it.
    pub fn config_id(&self) -> &str {
        &self.config_id
    }

    /// Stable fingerprint of the algorithm version and memory parameters only.
    /// Changing retention or interval limits leaves this identity unchanged.
    pub fn parameters_id(&self) -> &str {
        &self.parameters_id
    }

    /// Positive FSRS-6 forgetting-curve decay, for database sorting/aggregation.
    ///
    /// For validated reviewed memory with stability `s` and whole elapsed days `t`,
    /// recall is `(1 + t / s * (0.9^(-1 / decay) - 1))^(-decay)`.
    /// Use the stored configuration's decay, not the latest default. Unseen memory
    /// has no recall estimate. See `docs/review-storage.md` for SQL and test vectors.
    pub fn decay(&self) -> f32 {
        self.config.parameters[20]
    }

    pub fn new_state(&self, due_at: i64) -> ReviewState {
        ReviewState {
            version: self.config.version,
            memory: None,
            due_at,
            reviews: 0,
            lapses: 0,
            parameters_id: self.parameters_id.clone(),
            origin: ReviewOrigin::New,
            legacy: None,
        }
    }

    /// Applies one independent recall rating, including same-day reviews.
    /// The caller must deduplicate events; equal timestamps are allowed.
    /// `Again` increments the lapse counter even on the first review, preserving
    /// this crate's existing counter meaning. Short retries are application policy.
    pub fn review(&self, previous: &ReviewState, rating: RecallRating, now: i64) -> Result<ReviewTransition, Error> {
        self.check_state(previous)?;
        let elapsed_days = elapsed_days(previous.memory.map(|reviewed| reviewed.last_review_at), now)?;
        let next = self.next(previous.memory.map(|reviewed| reviewed.memory), rating, elapsed_days)?;
        if !next.interval.is_finite() {
            return Err(ReviewError::Backend("nonfinite interval".into()).into());
        }
        let interval_days = next
            .interval
            .round()
            .clamp(1.0, self.config.maximum_interval_days as f32) as u32;
        let due_at = now
            .checked_add(i64::from(interval_days) * DAY)
            .ok_or(ReviewError::TimestampOverflow)?;
        let state = ReviewState {
            memory: Some(ReviewedMemory {
                memory: MemoryState {
                    stability_days: next.memory.stability,
                    difficulty: next.memory.difficulty,
                },
                last_review_at: now,
            }),
            due_at,
            reviews: previous.reviews.saturating_add(1),
            lapses: previous.lapses.saturating_add(u32::from(rating == RecallRating::Again)),
            ..previous.clone()
        };
        state.validate()?;
        Ok(ReviewTransition {
            state,
            event: ReviewEvent { at: now, rating },
            elapsed_days,
            interval_days,
            config_id: self.config_id.clone(),
        })
    }

    /// Predicted recall for this exact item, using the same whole-day convention
    /// as scheduling. `None` for an item with no memory estimate. Not chess mastery.
    pub fn retrievability(&self, state: &ReviewState, now: i64) -> Result<Option<f32>, Error> {
        self.check_state(state)?;
        let elapsed_days = elapsed_days(state.memory.map(|reviewed| reviewed.last_review_at), now)?;
        Ok(state.memory.map(|reviewed| {
            fsrs::current_retrievability(into_backend(reviewed.memory), elapsed_days as f32, self.decay())
        }))
    }

    /// Rebuilds memory under this configuration from complete chronological
    /// history, or a caller-supplied seed followed by a partial history. Returns
    /// `None` for an empty history without a seed. The first unseeded event has
    /// elapsed time zero; same-day events are retained. No optimizer data is made.
    pub fn replay(&self, history: &[ReviewEvent], seed: Option<MemorySeed>) -> Result<Option<MemorySeed>, Error> {
        if let Some(seed) = seed {
            seed.memory.validate()?;
        }
        let mut current = seed;
        for event in history {
            let days = elapsed_days(current.map(|state| state.last_review_at), event.at)?;
            let memory = self.next(current.map(|state| state.memory), event.rating, days)?.memory;
            current = Some(MemorySeed {
                memory: MemoryState {
                    stability_days: memory.stability,
                    difficulty: memory.difficulty,
                },
                last_review_at: event.at,
            });
        }
        Ok(current)
    }

    /// Imports legacy progress without moving its due date or resetting counters.
    /// Without a seed, history must contain every counted legacy review (including
    /// failures, excluding inconclusive grades). With a seed, history is
    /// a known suffix and the result remains labeled estimated.
    ///
    /// An experienced item with no history requires an explicit seed. In
    /// particular, step 0 and saturated step 6 do not identify a prior interval.
    /// Keep such items on the application's legacy path until evidence or an
    /// explicit estimate is available; this function never invents review events.
    pub fn migrate(
        &self,
        previous: &LegacyReviewState,
        history: &[ReviewEvent],
        seed: Option<MemorySeed>,
    ) -> Result<ReviewState, Error> {
        let pending = ReviewState {
            reviews: previous.reviews,
            lapses: previous.lapses,
            legacy: Some(previous.clone()),
            ..self.new_state(previous.due_at)
        };
        self.restore(&pending, history, seed)
    }

    /// Rebuilds memory after changing parameters while retaining due dates,
    /// counters, and the migration snapshot. Supply complete history or an
    /// explicit approximate seed; never merely relabel old memory parameters.
    pub fn rebuild(
        &self,
        previous: &ReviewState,
        history: &[ReviewEvent],
        seed: Option<MemorySeed>,
    ) -> Result<ReviewState, Error> {
        previous.validate()?;
        let imported = self.restore(previous, history, seed)?;
        if imported.memory.map(|reviewed| reviewed.last_review_at)
            != previous.memory.map(|reviewed| reviewed.last_review_at)
        {
            return Err(ReviewError::InvalidHistory("replayed history must end at the stored last review time").into());
        }
        Ok(imported)
    }

    fn restore(
        &self,
        previous: &ReviewState,
        history: &[ReviewEvent],
        seed: Option<MemorySeed>,
    ) -> Result<ReviewState, Error> {
        let reviews = history.len();
        let lapses = history
            .iter()
            .filter(|event| event.rating == RecallRating::Again)
            .count();
        if previous.lapses > previous.reviews
            || reviews > previous.reviews as usize
            || lapses > previous.lapses as usize
            || reviews - lapses > (previous.reviews - previous.lapses) as usize
        {
            return Err(ReviewError::InvalidHistory("review counters do not match history").into());
        }
        if seed.is_none() && reviews != previous.reviews as usize {
            return Err(
                ReviewError::InvalidHistory("incomplete review history requires an explicit memory seed").into(),
            );
        }
        if seed.is_some() && reviews == previous.reviews as usize {
            return Err(ReviewError::InvalidHistory(
                "a memory seed requires at least one review before the supplied history",
            )
            .into());
        }
        let memory = self.replay(history, seed)?;
        let state = ReviewState {
            version: self.config.version,
            parameters_id: self.parameters_id.clone(),
            memory: memory.map(|state| ReviewedMemory {
                memory: state.memory,
                last_review_at: state.last_review_at,
            }),
            origin: if seed.is_some() {
                ReviewOrigin::Estimated
            } else {
                ReviewOrigin::Replayed
            },
            ..previous.clone()
        };
        state.validate()?;
        Ok(state)
    }

    fn next(&self, memory: Option<MemoryState>, rating: RecallRating, days: u32) -> Result<fsrs::ItemState, Error> {
        let next = self
            .model
            .next_states(memory.map(into_backend), self.config.desired_retention, days)
            .map_err(backend_error)?;
        Ok(match rating {
            RecallRating::Again => next.again,
            RecallRating::Hard => next.hard,
            RecallRating::Good => next.good,
            RecallRating::Easy => next.easy,
        })
    }

    fn check_state(&self, state: &ReviewState) -> Result<(), Error> {
        state.validate()?;
        if state.version != self.config.version || state.parameters_id != self.parameters_id {
            return Err(ReviewError::ParametersMismatch.into());
        }
        Ok(())
    }
}

fn elapsed_days(previous: Option<i64>, now: i64) -> Result<u32, Error> {
    let Some(previous) = previous else { return Ok(0) };
    if now < previous {
        return Err(ReviewError::NonChronologicalTime.into());
    }
    let seconds = now.checked_sub(previous).ok_or(ReviewError::TimestampOverflow)?;
    u32::try_from(seconds / DAY).map_err(|_| ReviewError::TimestampOverflow.into())
}

fn into_backend(memory: MemoryState) -> fsrs::MemoryState {
    fsrs::MemoryState {
        stability: memory.stability_days,
        difficulty: memory.difficulty,
    }
}

fn backend_error(error: fsrs::FSRSError) -> Error {
    ReviewError::Backend(error.to_string()).into()
}
