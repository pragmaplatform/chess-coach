use esca::{Game, classic, openings::Opening};

/// Identifies an opening family from a mainline of SAN moves using the bundled catalogue.
/// The last named position wins, including transpositions. Invalid moves stop classification
/// at the legal prefix; game analysis remains responsible for reporting illegal moves.
pub fn family(moves_san: &str) -> Option<&'static str> {
    classify(moves_san).map(|opening| {
        opening
            .name
            .split_once(':')
            .map_or(opening.name, |(family, _)| family)
            .trim()
    })
}

fn classify(moves_san: &str) -> Option<Opening> {
    let mut game = Game::new(classic());
    for san in moves_san.split_whitespace() {
        if game.play_san(san).is_err() {
            break;
        }
    }
    game.opening()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_families_from_moves_without_provider_titles() {
        for (moves, family) in [
            ("e4 c6 d4 d5", "Caro-Kann Defense"),
            ("e4 c5 Nf3 d6 d4 cxd4 Nxd4 Nf6 Nc3 g6", "Sicilian Defense"),
            ("e4 e5 Nf3 Nc6 Bb5", "Ruy Lopez"),
        ] {
            assert_eq!(super::family(moves).unwrap(), family);
        }
    }

    #[test]
    fn transpositions_and_later_moves_keep_the_last_named_position() {
        assert_eq!(classify("e4 e5 Nf3 Nc6 Bb5"), classify("Nf3 Nc6 e4 e5 Bb5"));
        assert_eq!(family("e4 c6 d4 d5 a3 h6").unwrap(), "Caro-Kann Defense");
    }

    #[test]
    fn invalid_games_keep_only_the_legal_prefix_classification() {
        assert_eq!(classify(""), None);
        assert_eq!(classify("invalid"), None);
        assert_eq!(classify("e4 e4"), classify("e4"));
    }
}
