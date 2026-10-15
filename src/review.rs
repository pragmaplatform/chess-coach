//! Memory scheduling, independent of move quality, storage, and practice sessions.
//!
//! Data types are available without features. Enable `fsrs` for `FsrsScheduler`.
//! Timestamps are UTC epoch seconds; elapsed days are whole 86,400-second periods
//! since the previous review, not calendar-day boundaries. Applications own event
//! identity, duplicate prevention, rating collection, and short practice retries.

use serde::{Deserialize, Serialize};

use crate::{Error, practice::Grade};

#[cfg(feature = "fsrs")]
mod scheduler;
#[cfg(feature = "fsrs")]
pub use scheduler::FsrsScheduler;

/// Machine-readable scheduling failures, wrapped in [`Error::Review`].
/// Serde decoding still reports malformed persisted input through its own error type.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReviewError {
    #[error("Invalid review configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("Invalid review state: {0}")]
    InvalidState(&'static str),
    #[error("Memory parameters changed: replay history with rebuild before scheduling")]
    ParametersMismatch,
    #[error("Review timestamps must be chronological")]
    NonChronologicalTime,
    #[error("Review timestamp or elapsed days overflow")]
    TimestampOverflow,
    #[error("Invalid review history: {0}")]
    InvalidHistory(&'static str),
    #[error("FSRS: {0}")]
    Backend(String),
}

/// Recall difficulty, independent of the engine's assessment of a move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessRecallRating))]
pub enum RecallRating {
    Again,
    Hard,
    Good,
    Easy,
}

impl RecallRating {
    /// Binary proxy when subjective recall difficulty was not collected.
    ///
    /// Both successful chess grades map to `Good`, never automatically to `Easy`
    /// or `Hard`. Teaching before an attempt is an exposure, not a failed review. An
    /// inconclusive grade must not change scheduling state or counters.
    pub fn from_grade(grade: Grade) -> Option<Self> {
        match grade {
            Grade::Correct | Grade::Good => Some(Self::Good),
            Grade::Incorrect => Some(Self::Again),
            Grade::Inconclusive => None,
        }
    }
}

/// Identifies both the state format and memory algorithm. Unknown versions fail
/// deserialization instead of silently starting the item over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessReviewVersion))]
pub enum ReviewVersion {
    Fsrs6V1,
}

/// A memory estimate is specific to one user's stable exercise/answer contract.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessMemoryState))]
pub struct MemoryState {
    /// Days until predicted recall falls to 90%; not a measure of chess mastery.
    pub stability_days: f32,
    /// FSRS item difficulty, from 1 to 10.
    pub difficulty: f32,
}

impl MemoryState {
    pub fn validate(&self) -> Result<(), Error> {
        if !(0.001..=36_500.0).contains(&self.stability_days) || !(1.0..=10.0).contains(&self.difficulty) {
            return Err(ReviewError::InvalidState("memory stability or difficulty is outside FSRS-6 bounds").into());
        }
        Ok(())
    }
}

/// A memory estimate together with the review time from which it decays.
/// These values are always present together; unseen items have neither.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessReviewedMemory))]
pub struct ReviewedMemory {
    pub memory: MemoryState,
    pub last_review_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessReviewOrigin))]
pub enum ReviewOrigin {
    New,
    Replayed,
    Estimated,
}

/// The former fixed-interval state, retained only for explicit migration/rollback.
/// Counts and steps cannot recover review history or the last review timestamp.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessLegacyReviewState))]
pub struct LegacyReviewState {
    pub step: u8,
    pub reviews: u32,
    pub lapses: u32,
    pub due_at: i64,
}

/// Persist per user and exercise. Persist the corresponding configuration too.
/// There is deliberately no `learned()` flag: memory retention is not transfer.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessReviewState))]
pub struct ReviewState {
    pub version: ReviewVersion,
    pub memory: Option<ReviewedMemory>,
    pub due_at: i64,
    pub reviews: u32,
    pub lapses: u32,
    /// Fingerprint of the algorithm and memory parameters, not the retention target.
    pub parameters_id: String,
    pub origin: ReviewOrigin,
    /// Original migration snapshot; subsequent reviews leave it unchanged.
    pub legacy: Option<LegacyReviewState>,
}

impl ReviewState {
    pub fn validate(&self) -> Result<(), Error> {
        if self.parameters_id.is_empty() || self.lapses > self.reviews {
            return Err(
                ReviewError::InvalidState("review state has no parameter identity or inconsistent counters").into(),
            );
        }
        match self.memory {
            Some(reviewed) if self.reviews > 0 && self.due_at >= reviewed.last_review_at => reviewed.memory.validate(),
            None if self.reviews == 0 => Ok(()),
            _ => Err(ReviewError::InvalidState("review memory, timestamps, and counters are inconsistent").into()),
        }
    }
}

impl<'de> Deserialize<'de> for ReviewState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            version: ReviewVersion,
            memory: Option<ReviewedMemory>,
            due_at: i64,
            reviews: u32,
            lapses: u32,
            parameters_id: String,
            origin: ReviewOrigin,
            legacy: Option<LegacyReviewState>,
        }
        let fields = Fields::deserialize(deserializer)?;
        let state = Self {
            version: fields.version,
            memory: fields.memory,
            due_at: fields.due_at,
            reviews: fields.reviews,
            lapses: fields.lapses,
            parameters_id: fields.parameters_id,
            origin: fields.origin,
            legacy: fields.legacy,
        };
        state.validate().map_err(serde::de::Error::custom)?;
        Ok(state)
    }
}

/// A qualifying recall event. The application additionally stores item/event IDs,
/// objective grade, rating provenance, assistance, and teaching exposures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessReviewEvent))]
pub struct ReviewEvent {
    pub at: i64,
    pub rating: RecallRating,
}

/// Explicit approximate state immediately before a partial review history.
/// Never submit this estimate as a fabricated review to a parameter optimizer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessMemorySeed))]
pub struct MemorySeed {
    pub memory: MemoryState,
    pub last_review_at: i64,
}

/// Versioned inputs for the pinned FSRS-6 implementation. Optimization is external.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessFsrsConfig))]
pub struct FsrsConfig {
    pub version: ReviewVersion,
    /// Exactly 21 finite FSRS-6 parameters. Upstream applies its parameter bounds.
    pub parameters: Vec<f32>,
    /// Allowed range 0.70–0.99; default 0.90. This is a target, not a guarantee.
    pub desired_retention: f32,
    pub maximum_interval_days: u32,
}

impl FsrsConfig {
    pub fn validate(&self) -> Result<(), Error> {
        if self.parameters.len() != 21 || self.parameters.iter().any(|value| !value.is_finite()) {
            return Err(ReviewError::InvalidConfiguration("expected 21 finite FSRS-6 parameters").into());
        }
        // Keep retrievability calculations consistent with the upstream decay bound.
        if !(0.1..=0.8).contains(&self.parameters[20]) {
            return Err(ReviewError::InvalidConfiguration("FSRS-6 decay must be between 0.1 and 0.8").into());
        }
        if !(0.70..=0.99).contains(&self.desired_retention) || !(1..=36_500).contains(&self.maximum_interval_days) {
            return Err(
                ReviewError::InvalidConfiguration("invalid desired retention or maximum review interval").into(),
            );
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for FsrsConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            version: ReviewVersion,
            parameters: Vec<f32>,
            desired_retention: f32,
            maximum_interval_days: u32,
        }
        let fields = Fields::deserialize(deserializer)?;
        let config = Self {
            version: fields.version,
            parameters: fields.parameters,
            desired_retention: fields.desired_retention,
            maximum_interval_days: fields.maximum_interval_days,
        };
        config.validate().map_err(serde::de::Error::custom)?;
        Ok(config)
    }
}

/// Persist the event and resulting state atomically. Configuration identity also
/// includes retention and interval limits, allowing decisions to be reproduced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessReviewTransition))]
pub struct ReviewTransition {
    pub state: ReviewState,
    pub event: ReviewEvent,
    pub elapsed_days: u32,
    pub interval_days: u32,
    pub config_id: String,
}
