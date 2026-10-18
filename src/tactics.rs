use cozy_chess::{Piece, util};
use serde::{Deserialize, Serialize};

use crate::{
    Error,
    analysis::Observation,
    engine::{Engine, EngineProfile, Evaluation, EvaluationConfidence, EvaluationRequest, MoveEvaluation, loss},
    game::{self, Position},
    teaching::material_balance,
};

pub const DETECTOR_VERSION: &str = "sound-piece-offer-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessTacticalScan))]
pub struct TacticalScan {
    pub detector_version: String,
    /// The two search budgets used for verification, distinct from normal analysis.
    pub profiles: Vec<EngineProfile>,
    pub opportunities: Vec<Opportunity>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessTacticalOpportunity))]
pub struct Opportunity {
    pub role: Role,
    pub move_uci: String,
    pub piece: SacrificedPiece,
    pub motifs: Vec<Motif>,
    /// Root is the original player decision; scores are from the player's perspective.
    pub sacrifice: Evaluation,
    /// Root after the offer. Both reply scores are from the opponent's perspective.
    pub reply_position: Position,
    pub best_defence: Evaluation,
    /// Captures the offered piece. The remaining PV shows the player's continuation.
    /// This is a capture heuristic, not a measured human response probability.
    pub capture: Evaluation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessTacticalRole))]
pub enum Role {
    MissedSacrifice,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessTacticalMotif))]
pub enum Motif {
    Sacrifice,
    Trap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessSacrificedPiece))]
pub enum SacrificedPiece {
    Knight,
    Bishop,
    Rook,
    Queen,
}

/// Cheap shortlist only: a missed best move offers a piece for a material deficit.
/// Does no engine work. Deliberately excludes pawn offers and already attacked pieces.
pub fn is_candidate(observation: &Observation) -> Result<bool, Error> {
    Ok(candidate(observation)?.is_some())
}

/// At most eight engine calls: best/candidate and best/capture at each of two budgets.
/// The caller chooses budgets, schedules this work, and may yield between engine calls.
/// Returns an empty scan when the shortlist or verification does not establish a sound offer.
pub async fn scan(
    observation: &Observation,
    profiles: [EngineProfile; 2],
    engine: &mut impl Engine,
) -> Result<TacticalScan, Error> {
    for profile in &profiles {
        profile.validate()?;
    }
    if profiles[1].nodes <= profiles[0].nodes
        || profiles[0].engine_name != profiles[1].engine_name
        || profiles[0].threads != profiles[1].threads
        || profiles[0].hash_mb != profiles[1].hash_mb
    {
        return Err(Error::Configuration(
            "Tactical confirmation must use the same engine with more nodes".into(),
        ));
    }
    let mut result = TacticalScan {
        detector_version: DETECTOR_VERSION.into(),
        profiles: profiles.to_vec(),
        opportunities: Vec::new(),
    };
    let Some(candidate) = candidate(observation)? else {
        return Ok(result);
    };
    let own = verified_comparison(
        engine,
        &observation.decision.position,
        &observation.best.best_move,
        &profiles,
    )
    .await?;
    // Keep only an engine-selected best move at the stronger budget. Grading remains BestMove.
    if own.confidence != EvaluationConfidence::Confirmed
        || own.best.best_move != observation.best.best_move
        || own.played.score.probability() < 0.5
    {
        return Ok(result);
    }
    let reply = verified_comparison(engine, &candidate.position, &candidate.capture, &profiles).await?;
    if reply.confidence != EvaluationConfidence::Confirmed
        || reply.best.score.probability() > 0.5
        || (1.0 - reply.best.score.probability() - own.played.score.probability()).abs() > 0.02
        || reply.played.line.len() < 2
    {
        return Ok(result);
    }
    // Exclude routine equal exchanges/recaptures: material remains sacrificed after our reply.
    let after_reply = game::position_after(&candidate.position, &reply.played.line[..2])?;
    let board = game::board(&observation.decision.position)?;
    if material_balance(&game::board(&after_reply)?, board.side_to_move())
        > material_balance(&board, board.side_to_move()) - 2
    {
        return Ok(result);
    }
    let mut motifs = vec![Motif::Sacrifice];
    let reply_board = game::board(&candidate.position)?;
    let best_reply = game::legal_move(&reply_board, &reply.best.best_move)?;
    let capture = game::legal_move(&reply_board, &candidate.capture)?;
    if best_reply.to != capture.to && loss(reply.best.score, reply.played.score) >= 0.10 {
        motifs.push(Motif::Trap);
    }
    result.opportunities.push(Opportunity {
        role: Role::MissedSacrifice,
        move_uci: observation.best.best_move.clone(),
        piece: candidate.piece,
        motifs,
        sacrifice: own.played,
        reply_position: candidate.position,
        best_defence: reply.best,
        capture: reply.played,
    });
    Ok(result)
}

struct Candidate {
    position: Position,
    capture: String,
    piece: SacrificedPiece,
}

fn candidate(observation: &Observation) -> Result<Option<Candidate>, Error> {
    if !observation.mistake
        || observation.confidence == EvaluationConfidence::Inconclusive
        || observation.best.best_move == observation.decision.played_uci
        || observation.best.score.probability() < 0.5
    {
        return Ok(None);
    }
    let board = game::board(&observation.decision.position)?;
    let mv = game::legal_move(&board, &observation.best.best_move)?;
    let piece = match board.piece_on(mv.from) {
        Some(Piece::Knight) => SacrificedPiece::Knight,
        Some(Piece::Bishop) => SacrificedPiece::Bishop,
        Some(Piece::Rook) => SacrificedPiece::Rook,
        Some(Piece::Queen) => SacrificedPiece::Queen,
        _ => return Ok(None),
    };
    if !board.checkers().is_empty()
        || board
            .null_move()
            .is_some_and(|opponent| opponent.generate_moves(|moves| moves.to.has(mv.from)))
    {
        return Ok(None);
    }
    let position = game::position_after(
        &observation.decision.position,
        std::slice::from_ref(&observation.best.best_move),
    )?;
    let offered = game::board(&position)?;
    let initial = material_balance(&board, board.side_to_move());
    let mut captures = Vec::new();
    offered.generate_moves(|moves| {
        for reply in moves {
            if reply.to == mv.to {
                let mut after = offered.clone();
                after.play(reply);
                if material_balance(&after, board.side_to_move()) <= initial - 2 {
                    let value = match offered.piece_on(reply.from) {
                        Some(Piece::Pawn) => 1,
                        Some(Piece::Knight | Piece::Bishop) => 3,
                        Some(Piece::Rook) => 5,
                        Some(Piece::Queen) => 9,
                        _ => 10,
                    };
                    captures.push((value, util::display_uci_move(&offered, reply).to_string()));
                }
            }
        }
        false
    });
    // Prefer the least valuable attacker; UCI order breaks ties deterministically.
    Ok(captures.into_iter().min().map(|(_, capture)| Candidate {
        position,
        capture,
        piece,
    }))
}

async fn verified_comparison(
    engine: &mut impl Engine,
    position: &Position,
    mv: &str,
    profiles: &[EngineProfile; 2],
) -> Result<MoveEvaluation, Error> {
    let initial = checked_comparison(engine, position, mv, &profiles[0]).await?;
    let confirmation = checked_comparison(engine, position, mv, &profiles[1]).await?;
    Ok(confirmation.compared_with(&initial))
}

async fn checked_comparison(
    engine: &mut impl Engine,
    position: &Position,
    mv: &str,
    profile: &EngineProfile,
) -> Result<MoveEvaluation, Error> {
    let comparison = crate::engine::evaluate_move(
        engine,
        &EvaluationRequest {
            position: position.clone(),
            profile: profile.clone(),
            root_move: Some(mv.into()),
        },
    )
    .await?;
    // Validate continuations even for custom Engine implementations.
    for evaluation in [&comparison.best, &comparison.played] {
        if evaluation.line.first() != Some(&evaluation.best_move) {
            return Err(Error::Engine(
                "Tactical evaluation is missing its principal variation".into(),
            ));
        }
        game::normalize_line(position, &evaluation.line)?;
    }
    if comparison.played.best_move != mv {
        return Err(Error::Engine("Tactical evaluation ignored the root move".into()));
    }
    Ok(comparison)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{Color, Game};

    #[test]
    fn capture_shortlist_prefers_a_pawn_over_an_alphabetically_earlier_queen() {
        let decision = game::decisions(
            &Game {
                id: "two-captures".into(),
                moves_san: "e4 e6 Bc4 d5 Bb3 a6 h3".into(),
                player_color: Color::White,
            },
            15,
        )
        .unwrap()
        .pop()
        .unwrap();
        let evaluation = Evaluation {
            source: Default::default(),
            score: crate::engine::Score::Centipawns(100),
            best_move: "b3d5".into(),
            line: vec!["b3d5".into()],
            depth: 20,
            nodes: 100,
        };
        let observation = Observation {
            decision,
            best: evaluation.clone(),
            played: evaluation,
            profile: EngineProfile {
                engine_name: "test".into(),
                threads: 1,
                hash_mb: 16,
                nodes: 100,
                timeout_ms: 1000,
            },
            loss: 0.2,
            confidence: EvaluationConfidence::Confirmed,
            teaching: None,
            severity: None,
            mistake: true,
            cloud_evaluations: None,
            tactics: None,
        };
        assert_eq!(candidate(&observation).unwrap().unwrap().capture, "e6d5");
    }
}
