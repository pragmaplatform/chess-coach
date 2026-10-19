use cozy_chess::{GameStatus, Piece, util};
use serde::{Deserialize, Serialize};

use crate::{
    Error,
    engine::{Engine, EngineProfile, Evaluation, EvaluationConfidence, EvaluationRequest, MoveEvaluation, Score, loss},
    game::{self, Position},
};

use super::{
    Candidate, Motif, SacrificedPiece, checked_comparison, move_candidate, validate_evaluation, validate_profiles,
    verified_motifs,
};

pub const POSITION_DETECTOR_VERSION: &str = "direct-piece-offer-v1";

/// An explicit scan of one game position, independent of mistake analysis and practice.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessPositionTacticalScan))]
pub struct PositionScan {
    pub detector_version: String,
    pub position: Position,
    pub profiles: Vec<EngineProfile>,
    /// Every geometrically eligible offer, in UCI order; not all are sound or confirmed.
    pub candidate_moves: Vec<String>,
    /// Unrestricted search at the stronger budget; absent when there are no candidates.
    pub best: Option<Evaluation>,
    pub opportunities: Vec<PositionOpportunity>,
}

/// A sound offer that may be the best move or give up an advantage while remaining equal or better.
/// Unlike an observation annotation, it does not imply the player missed or played the move.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessPositionTacticalOpportunity))]
pub struct PositionOpportunity {
    pub move_uci: String,
    pub piece: SacrificedPiece,
    pub motifs: Vec<Motif>,
    /// Restricted search from the original side-to-move's perspective.
    pub sacrifice: Evaluation,
    /// Opponent-to-move root used by both reply evaluations.
    pub reply_position: Position,
    pub best_defence: Evaluation,
    pub capture: Evaluation,
    /// Best minus candidate, clamped at zero for search noise. None if either score is a mate.
    pub advantage_given_up_cp: Option<i64>,
    /// Loss on the engine score's logistic scale, not a measured human win probability.
    pub loss: f64,
}

/// Enumerate legal direct piece offers without engine work, at any move number and for either side.
/// Includes moves by already attacked pieces and legal check evasions. Excludes pawns, kings,
/// promotions, and captures whose immediate material deficit is less than two pawn units.
pub fn candidate_moves(position: &Position) -> Result<Vec<String>, Error> {
    Ok(candidates(position)?
        .into_iter()
        .map(|candidate| candidate.move_uci)
        .collect())
}

/// Explicitly scan all direct piece offers; normal analysis and grading never call this function.
/// Uses at most 2 + 6N engine calls for N candidates, sharing both unrestricted root searches.
/// Both budgets must confirm a non-losing candidate and best defence, with consistent scores.
/// The offer and reply scores must stay stable; the unrestricted best score may change.
/// There is no maximum loss relative to the best move. Only one capture reply is tested per offer.
pub async fn scan_position(
    position: &Position,
    profiles: [EngineProfile; 2],
    engine: &mut impl Engine,
) -> Result<PositionScan, Error> {
    validate_profiles(&profiles)?;
    let candidates = candidates(position)?;
    let mut result = PositionScan {
        detector_version: POSITION_DETECTOR_VERSION.into(),
        position: position.clone(),
        profiles: profiles.to_vec(),
        candidate_moves: candidates.iter().map(|candidate| candidate.move_uci.clone()).collect(),
        best: None,
        opportunities: Vec::new(),
    };
    if candidates.is_empty() {
        return Ok(result);
    }
    let initial_best = evaluate(engine, position, None, &profiles[0]).await?;
    let best = evaluate(engine, position, None, &profiles[1]).await?;
    result.best = Some(best.clone());
    for candidate in candidates {
        let initial = comparison(engine, position, &candidate.move_uci, &profiles[0], &initial_best).await?;
        if initial.confidence == EvaluationConfidence::Inconclusive || initial.played.score.probability() < 0.5 {
            continue;
        }
        let current = comparison(engine, position, &candidate.move_uci, &profiles[1], &best).await?;
        let Some(own) = confirmed_offer(current, &initial) else {
            continue;
        };
        if own.score.probability() < 0.5 {
            continue;
        }
        let initial_reply = checked_comparison(engine, &candidate.position, &candidate.capture, &profiles[0]).await?;
        if initial_reply.confidence == EvaluationConfidence::Inconclusive
            || initial.played.source != initial_reply.best.source
            || !sound_reply(initial.played.score, initial_reply.best.score)
        {
            continue;
        }
        let current_reply = checked_comparison(engine, &candidate.position, &candidate.capture, &profiles[1]).await?;
        let Some(reply) = confirmed(current_reply, &initial_reply) else {
            continue;
        };
        if own.source != reply.best.source || !sound_reply(own.score, reply.best.score) {
            continue;
        }
        let Some(motifs) = verified_motifs(position, &candidate, &reply)? else {
            continue;
        };
        result.opportunities.push(PositionOpportunity {
            move_uci: candidate.move_uci,
            piece: candidate.piece,
            motifs,
            advantage_given_up_cp: match (best.score, own.score) {
                (Score::Centipawns(best), Score::Centipawns(played)) => {
                    Some((i64::from(best) - i64::from(played)).max(0))
                }
                _ => None,
            },
            loss: loss(best.score, own.score),
            sacrifice: own,
            reply_position: candidate.position,
            best_defence: reply.best,
            capture: reply.played,
        });
    }
    Ok(result)
}

fn candidates(position: &Position) -> Result<Vec<Candidate>, Error> {
    let board = game::board(position)?;
    if board.status() != GameStatus::Ongoing {
        return Ok(Vec::new());
    }
    let mut moves = Vec::new();
    board.generate_moves(|piece_moves| {
        let piece = match board.piece_on(piece_moves.from) {
            Some(Piece::Knight) => SacrificedPiece::Knight,
            Some(Piece::Bishop) => SacrificedPiece::Bishop,
            Some(Piece::Rook) => SacrificedPiece::Rook,
            Some(Piece::Queen) => SacrificedPiece::Queen,
            _ => return false,
        };
        moves.extend(
            piece_moves
                .into_iter()
                .map(|mv| (util::display_uci_move(&board, mv).to_string(), mv, piece)),
        );
        false
    });
    moves.sort_by(|a, b| a.0.cmp(&b.0));
    let mut candidates = Vec::new();
    for (_, mv, piece) in moves {
        if let Some(candidate) = move_candidate(position, mv, piece)? {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

async fn evaluate(
    engine: &mut impl Engine,
    position: &Position,
    root_move: Option<&str>,
    profile: &EngineProfile,
) -> Result<Evaluation, Error> {
    let evaluation = engine
        .evaluate(&EvaluationRequest {
            position: position.clone(),
            profile: profile.clone(),
            root_move: root_move.map(str::to_owned),
        })
        .await?;
    validate_evaluation(position, root_move, &evaluation)?;
    Ok(evaluation)
}

async fn comparison(
    engine: &mut impl Engine,
    position: &Position,
    mv: &str,
    profile: &EngineProfile,
    best: &Evaluation,
) -> Result<MoveEvaluation, Error> {
    let played = if mv == best.best_move {
        best.clone()
    } else {
        evaluate(engine, position, Some(mv), profile).await?
    };
    Ok(MoveEvaluation::new(best.clone(), played))
}

fn confirmed_offer(current: MoveEvaluation, previous: &MoveEvaluation) -> Option<Evaluation> {
    // The unrestricted best move is a loss reference, not the offer being confirmed.
    // Keep each search internally consistent, but compare only the offer across budgets.
    if previous.confidence == EvaluationConfidence::Inconclusive
        || current.confidence == EvaluationConfidence::Inconclusive
        || current.played.source != previous.played.source
        || current.played.score.mate_outcome() != previous.played.score.mate_outcome()
        || (current.played.score.probability() - previous.played.score.probability()).abs() > 0.02
    {
        return None;
    }
    Some(current.played)
}

fn confirmed(current: MoveEvaluation, previous: &MoveEvaluation) -> Option<MoveEvaluation> {
    if previous.confidence == EvaluationConfidence::Inconclusive
        || current.best.source != previous.best.source
        || current.played.source != previous.played.source
    {
        return None;
    }
    let current = current.compared_with(previous);
    (current.confidence == EvaluationConfidence::Confirmed).then_some(current)
}

fn sound_reply(own: Score, reply: Score) -> bool {
    reply.probability() <= 0.5
        && own.mate_outcome() == reply.mate_outcome().map(|winning| !winning)
        && (1.0 - reply.probability() - own.probability()).abs() <= 0.02
}
