use serde::{Deserialize, Serialize};

use crate::{
    Error,
    analysis::OpeningThreshold,
    engine::{Evaluation, EvaluationConfidence, EvaluationSource, MoveEvaluation, Score, loss, terminal_score},
    game::{self, Position},
    teaching::{self, Teaching, Theme},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessExercise))]
pub struct Exercise {
    pub position: Position,
    pub best: Evaluation,
    pub acceptable_loss: f64,
    /// Losses below this threshold can earn `Good`; absent in older, binary-graded exercises.
    #[serde(default)]
    pub minimum_loss: Option<f64>,
    /// Applies at this position's move number, including when copied into a follow-up.
    #[serde(default)]
    pub opening_threshold: Option<OpeningThreshold>,
    #[serde(default)]
    pub confidence: EvaluationConfidence,
    #[serde(default)]
    pub objective: Objective,
}

impl Exercise {
    pub fn minimum_loss(&self) -> Option<f64> {
        self.minimum_loss.map(|baseline| {
            self.opening_threshold
                .map_or(baseline, |threshold| threshold.minimum_loss(&self.position, baseline))
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessObjective))]
pub enum Objective {
    #[default]
    BestMove,
    FindMate,
    AvoidMate,
}

impl Objective {
    pub fn from_themes(themes: &[Theme]) -> Self {
        if themes.contains(&Theme::MissedMate) {
            Self::FindMate
        } else if themes.contains(&Theme::AllowedMate) {
            Self::AvoidMate
        } else {
            Self::BestMove
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessGrade))]
pub enum Grade {
    Correct,
    Good,
    Incorrect,
    Assisted,
    Inconclusive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessFeedback))]
pub struct Feedback {
    pub grade: Grade,
    pub loss: f64,
    pub best: Evaluation,
    pub played: Evaluation,
    pub continuation_san: Vec<String>,
    #[serde(default)]
    pub confidence: EvaluationConfidence,
    #[serde(default)]
    pub teaching: Option<Teaching>,
    #[serde(default)]
    pub objective_met: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessFollowUpPosition))]
pub struct FollowUpPosition {
    pub position: Position,
    pub setup_san: Vec<String>,
}

/// Search evidence for one position/history, engine identity, and submitted move.
/// Keep both searches when verification runs; confidence alone loses the cause of uncertainty.
#[derive(Clone, Debug)]
pub enum Evidence {
    /// A single search pair, not the result of `MoveEvaluation::compared_with`.
    Search(MoveEvaluation),
    /// Keep the earlier pair so grading can distinguish score drift from changed mate evidence.
    Verification {
        current: MoveEvaluation,
        previous: MoveEvaluation,
    },
}

impl From<MoveEvaluation> for Evidence {
    fn from(comparison: MoveEvaluation) -> Self {
        Self::Search(comparison)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Uncertainty {
    /// The submitted evaluation exceeds the root best beyond the ordinary tolerance.
    ScoreOrdering,
    /// The searches' centipawn estimates changed beyond the stability tolerance.
    ScoreDrift,
    /// A search pair disagrees in a way that involves a mating outcome.
    MateContradiction,
    /// The earlier and later searches disagree about a mating outcome.
    MateChanged,
    SourceMismatch,
    SubmittedMoveMismatch,
    UnsupportedObjective,
    /// A caller supplied inconclusive confidence without the underlying verification evidence.
    Unspecified,
}

#[derive(Clone, Debug)]
pub struct Assessment {
    pub comparison: MoveEvaluation,
    pub uncertainty: Option<Uncertainty>,
}

/// Assess practice evidence without changing the exercise's saved confidence.
/// Analysis/lesson discovery should continue using `MoveEvaluation::compared_with`.
pub fn assess(exercise: &Exercise, evidence: Evidence) -> Assessment {
    let (mut comparison, mut uncertainty) = match evidence {
        Evidence::Search(mut current) => {
            let mut reason = pair_uncertainty(&current).or_else(|| {
                (current.confidence == EvaluationConfidence::Inconclusive).then_some(Uncertainty::Unspecified)
            });
            if reason.is_some()
                && (current.best == current.played
                    || (reason == Some(Uncertainty::ScoreOrdering) && winning_mate_escape(exercise, &current)))
            {
                current.confidence = EvaluationConfidence::Unverified;
                reason = None;
            }
            (current, reason)
        }
        Evidence::Verification { current, previous } => {
            let reason = verification_uncertainty(&current, &previous);
            let mut verified = current.compared_with(&previous);
            let mut reason = reason.or_else(|| {
                (verified.confidence == EvaluationConfidence::Inconclusive).then_some(Uncertainty::Unspecified)
            });
            if matches!(reason, Some(Uncertainty::ScoreOrdering | Uncertainty::ScoreDrift))
                && (pair_uncertainty(&verified).is_none() || winning_mate_escape(exercise, &verified))
                && (verified.best == verified.played || acceptable_alternative(exercise, &verified, &previous))
            {
                verified.confidence = EvaluationConfidence::Unverified;
                reason = None;
            }
            (verified, reason)
        }
    };
    if !objective_supported(exercise.objective, comparison.best.score) {
        uncertainty = Some(Uncertainty::UnsupportedObjective);
    }
    if uncertainty.is_some() {
        comparison.confidence = EvaluationConfidence::Inconclusive;
    }
    Assessment {
        comparison,
        uncertainty,
    }
}

fn pair_uncertainty(comparison: &MoveEvaluation) -> Option<Uncertainty> {
    if MoveEvaluation::new(comparison.best.clone(), comparison.played.clone()).confidence
        != EvaluationConfidence::Inconclusive
    {
        None
    } else if comparison.best.source != comparison.played.source {
        Some(Uncertainty::SourceMismatch)
    } else if comparison.best.score.mate_outcome().is_some() || comparison.played.score.mate_outcome().is_some() {
        Some(Uncertainty::MateContradiction)
    } else {
        Some(Uncertainty::ScoreOrdering)
    }
}

fn verification_uncertainty(current: &MoveEvaluation, previous: &MoveEvaluation) -> Option<Uncertainty> {
    if current.played.best_move != previous.played.best_move {
        Some(Uncertainty::SubmittedMoveMismatch)
    } else if current.best.source != previous.best.source
        || current.played.source != previous.played.source
        || current.best.source != current.played.source
    {
        Some(Uncertainty::SourceMismatch)
    } else if current.best.score.mate_outcome() != previous.best.score.mate_outcome()
        || current.played.score.mate_outcome() != previous.played.score.mate_outcome()
    {
        Some(Uncertainty::MateChanged)
    } else if pair_uncertainty(current) == Some(Uncertainty::MateContradiction) {
        Some(Uncertainty::MateContradiction)
    } else if (current.best.score.probability() - previous.best.score.probability()).abs() > 0.02
        || (current.played.score.probability() - previous.played.score.probability()).abs() > 0.02
    {
        Some(Uncertainty::ScoreDrift)
    } else {
        pair_uncertainty(current)
    }
}

/// Compatibility helper for callers that copy the returned best/confidence into an exercise.
/// New integrations should retain `Evidence::Verification` and call `grade_with_evidence`
/// so a later grading call cannot lose the reason verification was inconclusive.
pub fn verify_comparison(exercise: &Exercise, current: MoveEvaluation, previous: &MoveEvaluation) -> MoveEvaluation {
    assess(
        exercise,
        Evidence::Verification {
            current,
            previous: previous.clone(),
        },
    )
    .comparison
}

fn acceptable_alternative(exercise: &Exercise, current: &MoveEvaluation, previous: &MoveEvaluation) -> bool {
    if (pair_uncertainty(previous).is_some() && !winning_mate_escape(exercise, previous))
        || current.best.source != previous.best.source
        || current.best.nodes <= previous.best.nodes
        || current.played.nodes <= previous.played.nodes
        || current.best.score.mate_outcome() != current.played.score.mate_outcome()
        || current.best.score.mate_outcome() != previous.best.score.mate_outcome()
        || current.played.score.mate_outcome() != previous.played.score.mate_outcome()
    {
        return false;
    }
    let cutoff = exercise.minimum_loss().unwrap_or(exercise.acceptable_loss);
    let current_loss = loss(current.best.score, current.played.score);
    let previous_loss = loss(previous.best.score, previous.played.score);
    let margin = 0.01;
    let tolerance = 0.02;
    let stable_loss = previous_loss < cutoff - margin && (current_loss - previous_loss).abs() <= tolerance;
    let borderline_revision = (previous_loss - cutoff).abs() <= margin
        && (current.played.score.probability() - previous.played.score.probability()).abs() <= tolerance;
    current_loss < cutoff - margin && (stable_loss || borderline_revision)
}

pub fn grade(exercise: &Exercise, submitted_uci: &str, played: Evaluation, assisted: bool) -> Result<Feedback, Error> {
    let mut comparison = MoveEvaluation::new(exercise.best.clone(), played);
    if comparison.confidence != EvaluationConfidence::Inconclusive {
        comparison.confidence = exercise.confidence;
    }
    grade_with_evidence(exercise, submitted_uci, Evidence::Search(comparison), assisted)
}

/// Grade raw or verified evidence while retaining the saved exercise and verification context.
/// Both searches must belong to this exercise's position/history and engine identity.
pub fn grade_with_evidence(
    exercise: &Exercise,
    submitted_uci: &str,
    evidence: Evidence,
    assisted: bool,
) -> Result<Feedback, Error> {
    game::legal_move(&game::board(&exercise.position)?, submitted_uci)?;
    let comparison = assess(exercise, evidence).comparison;
    if comparison.played.best_move != submitted_uci {
        return Err(Error::Engine("Evaluation is for another move".into()));
    }
    if comparison.best.source != comparison.played.source {
        return Err(Error::Engine(
            "Cannot grade scores from different evaluation sources".into(),
        ));
    }
    let loss = loss(comparison.best.score, comparison.played.score);
    let objective_met = match exercise.objective {
        Objective::BestMove => exercise
            .minimum_loss()
            .map_or(loss <= exercise.acceptable_loss, |minimum| loss < minimum),
        Objective::FindMate => matches!(comparison.played.score, Score::Mate(n) if n > 0),
        Objective::AvoidMate => !matches!(comparison.played.score, Score::Mate(n) if n <= 0),
    };
    let grade = if comparison.confidence == EvaluationConfidence::Inconclusive {
        Grade::Inconclusive
    } else if assisted {
        Grade::Assisted
    } else if !objective_met {
        Grade::Incorrect
    } else if loss <= exercise.acceptable_loss {
        Grade::Correct
    } else if exercise.minimum_loss().is_some_and(|minimum| loss < minimum) {
        Grade::Good
    } else {
        Grade::Incorrect
    };
    let teaching = teaching::explain(&exercise.position, &comparison)?;
    Ok(Feedback {
        grade,
        loss,
        continuation_san: teaching.best.moves_san.clone(),
        confidence: comparison.confidence,
        objective_met: (grade != Grade::Inconclusive).then_some(objective_met),
        teaching: Some(teaching),
        best: comparison.best,
        played: comparison.played,
    })
}

fn winning_mate_escape(exercise: &Exercise, comparison: &MoveEvaluation) -> bool {
    exercise.objective == Objective::AvoidMate
        && exercise.confidence != EvaluationConfidence::Inconclusive
        && comparison.best.source == EvaluationSource::Stockfish
        && comparison.played.source == EvaluationSource::Stockfish
        && matches!((comparison.best.score, comparison.played.score), (Score::Centipawns(_), Score::Centipawns(_)))
        // Define "clearly winning" using the score model, not a calibrated win probability.
        && comparison.best.score.probability() >= 0.90
        && comparison.played.score.is_better_than(comparison.best.score)
}

fn objective_supported(objective: Objective, best: Score) -> bool {
    match objective {
        Objective::BestMove => true,
        Objective::FindMate => matches!(best, Score::Mate(n) if n > 0),
        Objective::AvoidMate => !matches!(best, Score::Mate(n) if n <= 0),
    }
}

/// Continue a sound submitted line, or demonstrate the correction before asking for the next move.
/// The caller must evaluate this new position before issuing an exercise.
pub fn follow_up(position: &Position, feedback: &Feedback) -> Result<Option<FollowUpPosition>, Error> {
    if feedback.grade == Grade::Inconclusive || feedback.confidence == EvaluationConfidence::Inconclusive {
        return Ok(None);
    }
    let line = if matches!(feedback.grade, Grade::Correct | Grade::Good) {
        &feedback.played.line
    } else {
        &feedback.best.line
    };
    if line.len() < 3 {
        return Ok(None);
    }
    let child = game::position_after(position, &line[..2])?;
    if terminal_score(&child)?.is_some() {
        return Ok(None);
    }
    Ok(Some(FollowUpPosition {
        position: child,
        setup_san: game::line_san(position, &line[..2])?,
    }))
}
