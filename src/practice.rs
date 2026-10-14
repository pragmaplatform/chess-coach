use serde::{Deserialize, Serialize};

use crate::{
    Error,
    analysis::OpeningThreshold,
    engine::{Evaluation, EvaluationConfidence, MoveEvaluation, Score, loss, terminal_score},
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

/// Verify a stronger practice search without requiring absolute scores to converge.
///
/// Both comparisons must describe this exercise's position/history, engine identity,
/// and submitted move. Alternative-move recovery requires larger node counts for both
/// evaluations, matching sources, unchanged mating outcomes, and a final loss more
/// than one percentage point below the exercise's pass/fail cutoff. It accepts:
/// - consistently acceptable losses that change by at most two percentage points;
/// - an earlier borderline loss (within one point of the cutoff) when the submitted
///   move's evaluation changes by at most two points.
///
/// Recovered comparisons are `Unverified`: the grading evidence is sufficient, but
/// absolute scores did not converge. Copy the returned best/confidence into the
/// exercise before calling [`grade`]. Lesson discovery should continue using
/// [`MoveEvaluation::compared_with`].
pub fn verify_comparison(exercise: &Exercise, current: MoveEvaluation, previous: &MoveEvaluation) -> MoveEvaluation {
    let mut verified = current.compared_with(previous);
    if verified.played.best_move != previous.played.best_move || !consistent(&verified) {
        verified.confidence = EvaluationConfidence::Inconclusive;
        return verified;
    }
    if verified.confidence == EvaluationConfidence::Inconclusive
        && objective_supported(exercise.objective, verified.best.score)
        && (verified.best == verified.played || acceptable_alternative(exercise, &verified, previous))
    {
        verified.confidence = EvaluationConfidence::Unverified;
    }
    verified
}

fn consistent(comparison: &MoveEvaluation) -> bool {
    MoveEvaluation::new(comparison.best.clone(), comparison.played.clone()).confidence
        != EvaluationConfidence::Inconclusive
}

fn acceptable_alternative(exercise: &Exercise, current: &MoveEvaluation, previous: &MoveEvaluation) -> bool {
    if !consistent(previous)
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
    game::legal_move(&game::board(&exercise.position)?, submitted_uci)?;
    if played.best_move != submitted_uci {
        return Err(Error::Engine("Evaluation is for another move".into()));
    }
    if exercise.best.source != played.source {
        return Err(Error::Engine(
            "Cannot grade scores from different evaluation sources".into(),
        ));
    }
    let mut comparison = MoveEvaluation::new(exercise.best.clone(), played);
    if comparison.confidence != EvaluationConfidence::Inconclusive {
        comparison.confidence = exercise.confidence;
    }
    if comparison.best == comparison.played && comparison.confidence == EvaluationConfidence::Inconclusive {
        // The root search itself selected this answer. Absolute score drift does
        // not make a move worse than itself; do not claim the scores converged.
        comparison.confidence = EvaluationConfidence::Unverified;
    }
    if !objective_supported(exercise.objective, comparison.best.score) {
        comparison.confidence = EvaluationConfidence::Inconclusive;
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
