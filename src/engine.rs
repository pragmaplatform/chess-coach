use serde::{Deserialize, Serialize};

use crate::{Error, game::Position};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessEngineProfile))]
pub struct EngineProfile {
    pub engine_name: String,
    pub threads: u16,
    pub hash_mb: u32,
    pub nodes: u64,
    pub timeout_ms: u64,
}

impl EngineProfile {
    pub fn validate(&self) -> Result<(), Error> {
        if self.engine_name.is_empty()
            || self.threads == 0
            || self.hash_mb == 0
            || self.nodes == 0
            || self.timeout_ms == 0
        {
            return Err(Error::Configuration(
                "Engine limits and identity must be specified".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessEvaluationRequest))]
pub struct EvaluationRequest {
    /// Validated root position with its complete standard-chess move history.
    pub position: Position,
    pub profile: EngineProfile,
    /// Restrict the root search to this legal UCI move; `None` searches all legal moves.
    pub root_move: Option<String>,
}

/// Score from the root position's side-to-move perspective, never a fixed color.
/// Positive mate distances are winning; negative distances and zero are losing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessScore))]
pub enum Score {
    Centipawns(i32),
    Mate(i16),
}

impl Score {
    pub fn mate_outcome(self) -> Option<bool> {
        match self {
            Self::Mate(n) => Some(n > 0),
            Self::Centipawns(_) => None,
        }
    }

    pub fn is_better_than(self, other: Self) -> bool {
        match (self, other) {
            (Self::Centipawns(a), Self::Centipawns(b)) => a > b,
            (Self::Mate(a), Self::Mate(b)) if (a > 0) == (b > 0) => a < b,
            (Self::Mate(a), _) => a > 0,
            (_, Self::Mate(b)) => b <= 0,
        }
    }

    pub fn probability(self) -> f64 {
        match self {
            Self::Centipawns(cp) => 1.0 / (1.0 + (-0.00368 * f64::from(cp)).exp()),
            Self::Mate(moves) if moves > 0 => 1.0,
            Self::Mate(_) => 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessEvaluation))]
pub struct Evaluation {
    #[serde(default)]
    pub source: EvaluationSource,
    pub score: Score,
    pub best_move: String,
    pub line: Vec<String>,
    pub depth: u16,
    pub nodes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessEvaluationSource))]
pub enum EvaluationSource {
    #[default]
    Stockfish,
    LichessCloud,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessEvaluationConfidence))]
pub enum EvaluationConfidence {
    Inconclusive,
    #[default]
    Unverified,
    Confirmed,
}

#[derive(Clone, Debug)]
pub struct MoveEvaluation {
    pub best: Evaluation,
    pub played: Evaluation,
    pub confidence: EvaluationConfidence,
}

impl MoveEvaluation {
    pub fn new(best: Evaluation, played: Evaluation) -> Self {
        // Independent restricted searches can improve the score slightly. Mate discoveries
        // and inversions larger than one percentage point cannot be treated as search noise.
        let inconsistent = played.score.probability() - best.score.probability() > 0.01
            || matches!((played.score, best.score), (Score::Mate(n), Score::Centipawns(_)) if n > 0)
            || matches!((played.score, best.score), (Score::Centipawns(_), Score::Mate(n)) if n <= 0);
        let confidence = if inconsistent || best.source != played.source {
            EvaluationConfidence::Inconclusive
        } else {
            EvaluationConfidence::Unverified
        };
        Self {
            best,
            played,
            confidence,
        }
    }

    /// Searches agree on mating outcomes and both estimates change by at most two percentage points.
    pub fn compared_with(mut self, previous: &Self) -> Self {
        let stable = [
            (self.best.score, previous.best.score),
            (self.played.score, previous.played.score),
        ]
        .iter()
        .all(|(current, previous)| {
            current.mate_outcome() == previous.mate_outcome()
                && (current.probability() - previous.probability()).abs() <= 0.02
        });
        self.confidence = if self.confidence != EvaluationConfidence::Inconclusive && stable {
            EvaluationConfidence::Confirmed
        } else {
            EvaluationConfidence::Inconclusive
        };
        self
    }

    pub fn needs_confirmation(&self, thresholds: &[f64]) -> bool {
        self.confidence == EvaluationConfidence::Inconclusive
            || self.played.score.is_better_than(self.best.score)
            || self.best.score.mate_outcome() != self.played.score.mate_outcome()
            || thresholds.iter().any(|threshold| {
                ((threshold - 0.01)..=(threshold + 0.01)).contains(&loss(self.best.score, self.played.score))
            })
    }
}

/// Supplies evaluations for analysis and practice without requiring a particular transport.
#[async_trait::async_trait]
pub trait Engine: Send {
    /// Honor the root restriction, move history, and search profile. Return a legal
    /// continuation beginning with `best_move`, or an error if the search fails.
    async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error>;

    /// Optional cloud variations, best first, from the same root and search at the
    /// requested minimum depth. Mark them as `LichessCloud` and keep them separate
    /// from local searches. Return `None` when no suitable cached result exists.
    async fn cloud_evaluations(
        &mut self,
        _request: &EvaluationRequest,
        _minimum_depth: u16,
    ) -> Result<Option<Vec<Evaluation>>, Error> {
        Ok(None)
    }
}

pub async fn evaluate_move(engine: &mut impl Engine, request: &EvaluationRequest) -> Result<MoveEvaluation, Error> {
    let best = engine
        .evaluate(&EvaluationRequest {
            root_move: None,
            ..request.clone()
        })
        .await?;
    let played = if request.root_move.as_deref().is_none_or(|mv| mv == best.best_move) {
        best.clone()
    } else {
        engine.evaluate(request).await?
    };
    Ok(MoveEvaluation::new(best, played))
}

pub fn cloud_move(evaluations: &[Evaluation], played: &str, minimum_depth: u16) -> Option<MoveEvaluation> {
    let best = evaluations.first()?;
    let played = evaluations.iter().find(|evaluation| evaluation.best_move == played)?;
    [best, played]
        .iter()
        .all(|evaluation| evaluation.source == EvaluationSource::LichessCloud && evaluation.depth >= minimum_depth)
        .then(|| MoveEvaluation::new(best.clone(), played.clone()))
}

pub fn loss(best: Score, played: Score) -> f64 {
    (best.probability() - played.probability()).max(0.0)
}

pub fn terminal_score(position: &Position) -> Result<Option<Score>, Error> {
    Ok(match crate::game::board(position)?.status() {
        cozy_chess::GameStatus::Won => Some(Score::Mate(0)),
        cozy_chess::GameStatus::Drawn => Some(Score::Centipawns(0)),
        cozy_chess::GameStatus::Ongoing => None,
    })
}
