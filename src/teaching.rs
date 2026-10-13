use cozy_chess::{Board, Color, Piece, util};
use serde::{Deserialize, Serialize};

use crate::{
    Error,
    engine::{Evaluation, EvaluationConfidence, MoveEvaluation, Score},
    game::{self, Position},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessTheme))]
pub enum Theme {
    MissedMate,
    AllowedMate,
    MaterialLoss,
    MissedMaterialGain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessLineEvidence))]
pub struct LineEvidence {
    pub moves_san: Vec<String>,
    /// Change in the player's material balance at the end of this line, in pawn units.
    pub material_change: i16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessTeaching))]
pub struct Teaching {
    pub explanation: String,
    pub themes: Vec<Theme>,
    pub best: LineEvidence,
    pub played: LineEvidence,
}

pub fn explain(position: &Position, comparison: &MoveEvaluation) -> Result<Teaching, Error> {
    let best = line_evidence(position, &comparison.best)?;
    let played = line_evidence(position, &comparison.played)?;
    if comparison.confidence == EvaluationConfidence::Inconclusive {
        return Ok(Teaching {
            explanation: "The evaluations have not settled enough to judge this move.".into(),
            themes: Vec::new(),
            best,
            played,
        });
    }
    let best_move = best
        .moves_san
        .first()
        .ok_or_else(|| Error::Engine("Missing best continuation".into()))?;
    let played_move = played
        .moves_san
        .first()
        .ok_or_else(|| Error::Engine("Missing played continuation".into()))?;
    let mut themes = Vec::new();
    let mut reasons = Vec::new();
    let different_moves = comparison.best.best_move != comparison.played.best_move;
    if different_moves
        && matches!(comparison.best.score, Score::Mate(n) if n > 0)
        && !matches!(comparison.played.score, Score::Mate(n) if n > 0)
    {
        themes.push(Theme::MissedMate);
        reasons.push(format!(
            "The engine finds a forced mate with {best_move}; {played_move} does not retain a confirmed mating line."
        ));
    }
    if different_moves
        && matches!(comparison.played.score, Score::Mate(n) if n <= 0)
        && !matches!(comparison.best.score, Score::Mate(n) if n <= 0)
    {
        themes.push(Theme::AllowedMate);
        reasons.push(format!(
            "After {played_move}, the engine finds a forced mate against you."
        ));
    }
    if different_moves
        && comparison.best.score.is_better_than(comparison.played.score)
        && played.material_change < best.material_change
    {
        let theme = if played.material_change < 0 {
            Theme::MaterialLoss
        } else {
            Theme::MissedMaterialGain
        };
        themes.push(theme);
        reasons.push(format!(
            "At the end of the shown lines, {best_move} leaves you {} points of material better off than {played_move}.",
            best.material_change - played.material_change
        ));
    }
    if reasons.is_empty() {
        reasons.push(if !different_moves {
            format!("{played_move} matches the engine's preferred move. Continue the line to see the opponent's defense.")
        } else {
            format!(
                "Compare the continuation after {played_move} with {best_move}. These lines do not establish a specific tactical explanation."
            )
        });
    }
    if let Some(reply) = played.moves_san.get(1) {
        reasons.push(format!("The engine's reply to {played_move} is {reply}."));
    }
    Ok(Teaching {
        explanation: reasons.join(" "),
        themes,
        best,
        played,
    })
}

fn line_evidence(position: &Position, evaluation: &Evaluation) -> Result<LineEvidence, Error> {
    if evaluation.line.first() != Some(&evaluation.best_move) {
        return Err(Error::Engine(
            "Continuation does not start with the evaluated move".into(),
        ));
    }
    let mut board = game::board(position)?;
    let player = board.side_to_move();
    let initial = material_balance(&board, player);
    let moves_san = evaluation
        .line
        .iter()
        .map(|uci| {
            let mv = game::legal_move(&board, uci)?;
            let san = util::display_san_move(&board, mv).to_string();
            board.play(mv);
            Ok(san)
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(LineEvidence {
        moves_san,
        material_change: material_balance(&board, player) - initial,
    })
}

fn material_balance(board: &Board, player: Color) -> i16 {
    [
        (Piece::Pawn, 1),
        (Piece::Knight, 3),
        (Piece::Bishop, 3),
        (Piece::Rook, 5),
        (Piece::Queen, 9),
    ]
    .iter()
    .map(|&(piece, value)| {
        value * (board.colored_pieces(player, piece).len() as i16 - board.colored_pieces(!player, piece).len() as i16)
    })
    .sum()
}
