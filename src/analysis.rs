use serde::{Deserialize, Serialize};

use crate::{
    Error,
    engine::{
        Engine, EngineProfile, Evaluation, EvaluationConfidence, EvaluationRequest, cloud_move, evaluate_move, loss,
    },
    game::{self, Decision, Game, Position},
    teaching::{self, Teaching, Theme},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessAnalysisPolicy))]
pub struct AnalysisPolicy {
    pub version: String,
    pub full_moves: u16,
    pub minimum_loss: f64,
    /// Higher cutoff for early opening preferences; absent in older policies.
    #[serde(default)]
    pub opening_threshold: Option<OpeningThreshold>,
    pub acceptable_loss: f64,
    pub engine: EngineProfile,
    pub confirmation_nodes: u64,
    /// Final search budget for candidates; absent in older saved policies.
    #[serde(default)]
    pub verification_nodes: Option<u64>,
    /// Absent in older policies, which continue to use local searches only.
    #[serde(default)]
    pub cloud_min_depth: Option<u16>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessOpeningThreshold))]
pub struct OpeningThreshold {
    pub full_moves: u16,
    pub minimum_loss: f64,
}

impl Default for OpeningThreshold {
    fn default() -> Self {
        Self {
            full_moves: 5,
            minimum_loss: 0.10,
        }
    }
}

impl OpeningThreshold {
    pub fn minimum_loss(&self, position: &Position, baseline: f64) -> f64 {
        if position.history_uci.len() < usize::from(self.full_moves) * 2 {
            baseline.max(self.minimum_loss)
        } else {
            baseline
        }
    }
}

impl AnalysisPolicy {
    pub fn validate(&self) -> Result<(), Error> {
        self.engine.validate()?;
        if self.full_moves == 0
            || self.full_moves > 100
            || !self.minimum_loss.is_finite()
            || !(0.0..=1.0).contains(&self.minimum_loss)
            || !self.acceptable_loss.is_finite()
            || !(0.0..self.minimum_loss).contains(&self.acceptable_loss)
            || self.confirmation_nodes < self.engine.nodes
            || self.cloud_min_depth == Some(0)
            || self.opening_threshold.is_some_and(|threshold| {
                threshold.full_moves == 0
                    || threshold.full_moves > 100
                    || !threshold.minimum_loss.is_finite()
                    || !(self.minimum_loss..=1.0).contains(&threshold.minimum_loss)
            })
            || self
                .verification_nodes
                .is_some_and(|nodes| nodes <= self.confirmation_nodes)
        {
            return Err(Error::Configuration("Invalid analysis policy".into()));
        }
        Ok(())
    }

    pub fn confirmation_profile(&self) -> EngineProfile {
        EngineProfile {
            nodes: self.confirmation_nodes,
            ..self.engine.clone()
        }
    }

    pub fn verification_profile(&self) -> Option<EngineProfile> {
        self.verification_nodes.map(|nodes| EngineProfile {
            nodes,
            ..self.engine.clone()
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessSeverity))]
pub enum Severity {
    Inaccuracy,
    Mistake,
    Blunder,
}

impl Severity {
    pub fn from_loss(loss: f64) -> Option<Self> {
        if loss >= 0.15 {
            Some(Self::Blunder)
        } else if loss >= 0.10 {
            Some(Self::Mistake)
        } else if loss >= 0.05 {
            Some(Self::Inaccuracy)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessObservation))]
pub struct Observation {
    pub decision: Decision,
    pub best: Evaluation,
    pub played: Evaluation,
    pub profile: EngineProfile,
    pub loss: f64,
    #[serde(default)]
    pub confidence: EvaluationConfidence,
    #[serde(default)]
    pub teaching: Option<Teaching>,
    /// Null for losses below five percentage points or observations saved before severity labels.
    #[serde(default)]
    pub severity: Option<Severity>,
    /// Whether this observation is eligible for practice through outcome loss or a mating lesson.
    pub mistake: bool,
    /// Qualified cloud variations retained together for consistent practice grading.
    #[serde(default)]
    pub cloud_evaluations: Option<Vec<Evaluation>>,
}

pub async fn analyze_game(
    game: &Game,
    policy: &AnalysisPolicy,
    engine: &mut impl Engine,
) -> Result<Vec<Observation>, Error> {
    policy.validate()?;
    let mut observations = Vec::new();
    for decision in game::decisions(game, policy.full_moves)? {
        let minimum_loss = policy.opening_threshold.map_or(policy.minimum_loss, |threshold| {
            threshold.minimum_loss(&decision.position, policy.minimum_loss)
        });
        let mut request = EvaluationRequest {
            position: decision.position.clone(),
            profile: policy.engine.clone(),
            root_move: Some(decision.played_uci.clone()),
        };
        let cloud_evaluations = if let Some(depth) = policy.cloud_min_depth
            && game::cloud_fen(&decision.position)?.is_some()
        {
            engine.cloud_evaluations(&request, depth).await?
        } else {
            None
        };
        let cloud = cloud_evaluations
            .as_deref()
            .and_then(|evaluations| cloud_move(evaluations, &decision.played_uci, policy.cloud_min_depth?));
        let comparison = if let Some(comparison) = cloud.as_ref()
            && !comparison.needs_confirmation(&[minimum_loss])
        {
            request.profile = policy.confirmation_profile();
            comparison.clone()
        } else {
            if cloud.is_some() {
                request.profile = policy.confirmation_profile();
            }
            let mut comparison = evaluate_move(engine, &request).await?;
            if request.profile.nodes < policy.confirmation_nodes
                && (loss(comparison.best.score, comparison.played.score) >= minimum_loss / 2.0
                    || comparison.played.score.is_better_than(comparison.best.score)
                    || comparison.best.score.mate_outcome() != comparison.played.score.mate_outcome())
            {
                request.profile = policy.confirmation_profile();
                comparison = evaluate_move(engine, &request).await?.compared_with(&comparison);
            }
            if request.profile.nodes >= policy.confirmation_nodes
                && comparison.needs_confirmation(&[minimum_loss])
                && let Some(verification) = policy.verification_profile()
            {
                request.profile = verification;
                comparison = evaluate_move(engine, &request).await?.compared_with(&comparison);
            }
            comparison
        };
        let loss = loss(comparison.best.score, comparison.played.score);
        let conclusive = comparison.confidence != EvaluationConfidence::Inconclusive;
        let teaching = teaching::explain(&decision.position, &comparison)?;
        let tactical_lesson = teaching
            .themes
            .iter()
            .any(|theme| matches!(theme, Theme::MissedMate | Theme::AllowedMate));
        observations.push(Observation {
            decision,
            best: comparison.best,
            played: comparison.played,
            profile: request.profile,
            loss,
            confidence: comparison.confidence,
            teaching: Some(teaching),
            severity: if conclusive { Severity::from_loss(loss) } else { None },
            mistake: conclusive && (loss >= minimum_loss || tactical_lesson),
            cloud_evaluations,
        });
    }
    Ok(observations)
}
