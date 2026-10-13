use cozy_chess::{Board, Color as BoardColor, util};
use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessColor))]
pub enum Color {
    White,
    Black,
}

impl From<BoardColor> for Color {
    fn from(value: BoardColor) -> Self {
        match value {
            BoardColor::White => Self::White,
            BoardColor::Black => Self::Black,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessGame))]
pub struct Game {
    pub id: String,
    pub moves_san: String,
    pub player_color: Color,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessPosition))]
pub struct Position {
    pub fen: String,
    pub key: String,
    pub history_uci: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessDecision))]
pub struct Decision {
    pub game_id: String,
    pub ply: u16,
    pub color: Color,
    pub position: Position,
    pub played_uci: String,
    pub played_san: String,
}

pub fn decisions(game: &Game, full_moves: u16) -> Result<Vec<Decision>, Error> {
    let mut board = Board::default();
    let mut history = Vec::new();
    let mut result = Vec::new();
    for (index, san) in game.moves_san.split_whitespace().enumerate() {
        let mv = util::parse_san_move(&board, san)
            .map_err(|_| Error::InvalidGame(format!("{}: illegal move {san} at ply {}", game.id, index + 1)))?;
        let uci = util::display_uci_move(&board, mv).to_string();
        let color = Color::from(board.side_to_move());
        if index < usize::from(full_moves) * 2 && color == game.player_color {
            result.push(Decision {
                game_id: game.id.clone(),
                ply: (index + 1) as u16,
                color,
                position: Position {
                    fen: board.to_string(),
                    key: position_key(&board),
                    history_uci: history.clone(),
                },
                played_uci: uci.clone(),
                played_san: util::display_san_move(&board, mv).to_string(),
            });
        }
        board.play(mv);
        history.push(uci);
    }
    if history.is_empty() {
        return Err(Error::InvalidGame("Game has no moves".into()));
    }
    Ok(result)
}

pub fn board(position: &Position) -> Result<Board, Error> {
    let mut board = Board::default();
    for uci in &position.history_uci {
        let mv = legal_move(&board, uci)?;
        board.play(mv);
    }
    if board.to_string() != position.fen || position_key(&board) != position.key {
        return Err(Error::InvalidGame("Position does not match its history".into()));
    }
    Ok(board)
}

pub fn legal_move(board: &Board, uci: &str) -> Result<cozy_chess::Move, Error> {
    let mv = util::parse_uci_move(board, uci).map_err(|_| Error::InvalidGame(format!("Invalid UCI move: {uci}")))?;
    if !board.is_legal(mv) {
        return Err(Error::InvalidGame(format!("Illegal move: {uci}")));
    }
    Ok(mv)
}

pub fn position_after(position: &Position, moves: &[String]) -> Result<Position, Error> {
    let mut board = board(position)?;
    for uci in moves {
        let mv = legal_move(&board, uci)?;
        board.play(mv);
    }
    Ok(Position {
        fen: board.to_string(),
        key: position_key(&board),
        history_uci: position.history_uci.iter().chain(moves).cloned().collect(),
    })
}

pub fn line_san(position: &Position, moves: &[String]) -> Result<Vec<String>, Error> {
    let mut board = board(position)?;
    moves
        .iter()
        .map(|uci| {
            let mv = legal_move(&board, uci)?;
            let san = util::display_san_move(&board, mv).to_string();
            board.play(mv);
            Ok(san)
        })
        .collect()
}

/// A pawn move or capture makes earlier history irrelevant to repetition and resets the draw clock.
pub fn cloud_fen(position: &Position) -> Result<Option<String>, Error> {
    let board = board(position)?;
    Ok(
        (board.halfmove_clock() == 0 && board.status() == cozy_chess::GameStatus::Ongoing)
            .then(|| format!("{} 0 1", position.key)),
    )
}

pub fn normalize_line(position: &Position, moves: &[String]) -> Result<Vec<String>, Error> {
    let mut board = board(position)?;
    moves
        .iter()
        .map(|uci| {
            let mv = legal_move(&board, uci)?;
            let normalized = util::display_uci_move(&board, mv).to_string();
            board.play(mv);
            Ok(normalized)
        })
        .collect()
}

fn position_key(board: &Board) -> String {
    let fen = board.to_string();
    let mut parts: Vec<&str> = fen.split_whitespace().take(4).collect();
    if let Some(file) = board.en_passant() {
        let ep_square = cozy_chess::Square::new(file, cozy_chess::Rank::Sixth.relative_to(board.side_to_move()));
        let mut legal_ep = false;
        board.generate_moves(|moves| {
            if board.piece_on(moves.from) == Some(cozy_chess::Piece::Pawn) && moves.to.has(ep_square) {
                legal_ep = true;
            }
            false
        });
        if !legal_ep {
            parts[3] = "-";
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replays_both_sides_and_caps_at_full_move_fifteen() {
        let moves = "e4 e5 Nf3 Nc6 Bb5 a6 Ba4 Nf6 O-O Be7 Re1 b5 Bb3 d6 c3 O-O h3 Nb8 d4 Nbd7 c4 c6 cxb5 axb5 Nc3 Bb7 Bg5 b4 Nb1 h6 Bh4";
        for color in [Color::White, Color::Black] {
            let rows = decisions(
                &Game {
                    id: "game".into(),
                    moves_san: moves.into(),
                    player_color: color,
                },
                15,
            )
            .unwrap();
            assert_eq!(rows.len(), 15);
            for row in rows {
                assert!(board(&row.position).is_ok());
            }
        }
    }

    #[test]
    fn transpositions_share_teaching_key_but_keep_history() {
        let get = |moves: &str| {
            decisions(
                &Game {
                    id: "game".into(),
                    moves_san: moves.into(),
                    player_color: Color::White,
                },
                15,
            )
            .unwrap()
            .pop()
            .unwrap()
        };
        let a = get("Nf3 Nf6 g3 g6 Bg2");
        let b = get("g3 g6 Nf3 Nf6 Bg2");
        assert_eq!(a.position.key, b.position.key);
        assert_ne!(a.position.history_uci, b.position.history_uci);
        assert!(
            decisions(
                &Game {
                    id: "bad".into(),
                    moves_san: "e4 e4".into(),
                    player_color: Color::White
                },
                15
            )
            .is_err()
        );
    }
}
