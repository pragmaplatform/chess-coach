// Phase heuristics adapted from scalachess's MIT-licensed Divider.
// Source and copyright notice: docs/third-party-licenses.md.
use cozy_chess::{BitBoard, Board, Color, Piece, Rank};
use serde::{Deserialize, Serialize};

use crate::{
    Error,
    game::{self, Position},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessPhase))]
pub enum Phase {
    Opening,
    Middlegame,
    Endgame,
}

/// Classify a validated standard-game position using Lichess's material, development,
/// and mixedness thresholds. Replays both sides' moves to preserve phase transitions:
/// returning pieces home or promoting cannot revert to an earlier phase.
/// The phase describes this position, before the next move. No engine is needed.
pub fn classify(position: &Position) -> Result<Phase, Error> {
    // Validate the entire history and its FEN/key even if an early position reaches endgame.
    game::board(position)?;
    let mut board = Board::default();
    let mut phase = Phase::Opening;
    for uci in &position.history_uci {
        let mv = game::legal_move(&board, uci)?;
        board.play(mv);
        phase = advance(phase, &board);
        if phase == Phase::Endgame {
            break;
        }
    }
    Ok(phase)
}

fn advance(phase: Phase, board: &Board) -> Phase {
    let pieces = (board.occupied() & !(board.pieces(Piece::Pawn) | board.pieces(Piece::King))).len();
    if phase == Phase::Endgame || pieces <= 6 {
        Phase::Endgame
    } else if phase == Phase::Middlegame
        || pieces <= 10
        || (board.colors(Color::White) & Rank::First.bitboard()).len() < 4
        || (board.colors(Color::Black) & Rank::Eighth.bitboard()).len() < 4
        || mixedness(board) > 150
    {
        Phase::Middlegame
    } else {
        Phase::Opening
    }
}

fn mixedness(board: &Board) -> i32 {
    let mut total = 0;
    for rank in 0..7 {
        for file in 0..7 {
            let region = BitBoard(0x0303 << (rank * 8 + file));
            total += region_score(
                rank + 1,
                (board.colors(Color::White) & region).len(),
                (board.colors(Color::Black) & region).len(),
            );
        }
    }
    total
}

fn region_score(rank: i32, white: u32, black: u32) -> i32 {
    match (white, black) {
        (0, 1) => 1 + rank,
        (0, 2) if rank < 6 => 2 + (6 - rank),
        (0, 3 | 4) if rank < 7 => 3 + (7 - rank),
        (1, 0) => 1 + (8 - rank),
        (1, 1) => 5 + (4 - rank).abs(),
        (1, 2) => 4 + (7 - rank),
        (1, 3) => 5 + (7 - rank),
        (2, 0) if rank > 2 => 2 + (rank - 2),
        (2, 1) => 4 + (rank - 1),
        (2, 2) => 7,
        (3 | 4, 0) if rank > 1 => 3 + (rank - 1),
        (3, 1) => 5 + (rank - 1),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_thresholds_count_both_sides_and_exclude_pawns_and_kings() {
        for (fen, expected) in [
            (
                "r2qkbnr/pppppppp/8/8/8/8/PPPPPPPP/R1BQKBNR w KQkq - 0 1",
                Phase::Opening,
            ),
            (
                "r2qkbnr/pppppppp/8/8/8/8/PPPPPPPP/R2QKBNR w KQkq - 0 1",
                Phase::Middlegame,
            ),
            ("3qkbnr/pppppppp/8/8/8/8/PPPPPPPP/3QKBN1 w k - 0 1", Phase::Middlegame),
            ("3qkbn1/pppppppp/8/8/8/8/PPPPPPPP/3QKBN1 w - - 0 1", Phase::Endgame),
        ] {
            let board: Board = fen.parse().unwrap();
            assert!(mixedness(&board) <= 150);
            assert_eq!(advance(Phase::Opening, &board), expected, "{fen}");
        }
    }

    #[test]
    fn phases_never_revert_even_when_material_or_development_reverses() {
        let board = Board::default();
        assert_eq!(advance(Phase::Opening, &board), Phase::Opening);
        assert_eq!(advance(Phase::Middlegame, &board), Phase::Middlegame);
        assert_eq!(advance(Phase::Endgame, &board), Phase::Endgame);
    }

    #[test]
    fn sparse_home_rank_for_either_color_starts_middlegame() {
        for (fen, expected) in [
            (
                "rnbqkbnr/pppppppp/8/8/8/2NBBN2/PPP1PPPP/R2QK2R w KQkq - 0 1",
                Phase::Opening,
            ),
            (
                "rnbqkbnr/pppppppp/8/8/8/2NBBN2/PPPQPPPP/R3K2R w KQkq - 0 1",
                Phase::Middlegame,
            ),
            (
                "r2qk2r/ppp1pppp/2nbbn2/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
                Phase::Opening,
            ),
            (
                "r3k2r/pppqpppp/2nbbn2/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1",
                Phase::Middlegame,
            ),
        ] {
            let board: Board = fen.parse().unwrap();
            assert!(mixedness(&board) <= 150);
            assert_eq!(advance(Phase::Opening, &board), expected);
        }
    }

    #[test]
    fn mixedness_requires_strictly_more_than_150() {
        // All fourteen major/minor pieces remain on their home ranks, so only
        // mixedness can trigger this boundary. Scores checked against Divider.scala.
        for (fen, score, expected) in [
            (
                "rnbqkbnr/P3p1p1/5P2/1ppP3P/pp5p/2P4p/P2P2P1/RNBQKBNR w KQkq - 0 1",
                150,
                Phase::Opening,
            ),
            (
                "rnbqkbnr/1p1p2P1/p4p2/1p3p2/p3P1P1/2P2p2/PP1P1P2/RNBQKBNR w KQkq - 0 1",
                151,
                Phase::Middlegame,
            ),
        ] {
            let board: Board = fen.parse().unwrap();
            assert_eq!(mixedness(&board), score);
            assert_eq!(advance(Phase::Opening, &board), expected);
        }
    }
}
