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
    let objective_supported = match exercise.objective {
        Objective::BestMove => true,
        Objective::FindMate => matches!(comparison.best.score, Score::Mate(n) if n > 0),
        Objective::AvoidMate => !matches!(comparison.best.score, Score::Mate(n) if n <= 0),
    };
    if !objective_supported {
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
