use chess_coach::{
    game::{self, Color, Game},
    teaching::Theme,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    id: String,
    player_color: Color,
    moves_san: String,
    acceptable: Vec<String>,
    mistakes: Vec<Mistake>,
}

#[derive(Deserialize)]
struct Mistake {
    uci: String,
    theme: Option<Theme>,
}

fn corpus() -> Vec<Case> {
    serde_json::from_str(include_str!("fixtures/quality.json")).unwrap()
}

fn position(case: &Case) -> game::Position {
    game::decisions(
        &Game {
            id: case.id.clone(),
            player_color: case.player_color,
            moves_san: case.moves_san.clone(),
        },
        15,
    )
    .unwrap()
    .pop()
    .unwrap()
    .position
}

#[test]
fn quality_corpus_contains_legal_and_distinct_answers() {
    let cases = corpus();
    assert!(
        cases
            .iter()
            .flat_map(|case| &case.mistakes)
            .any(|mistake| mistake.theme.is_some())
    );
    for case in cases {
        let board = game::board(&position(&case)).unwrap();
        assert!(!case.acceptable.is_empty());
        for uci in case.acceptable.iter().chain(case.mistakes.iter().map(|m| &m.uci)) {
            game::legal_move(&board, uci).unwrap_or_else(|error| panic!("{}: {error}", case.id));
        }
        for mistake in case.mistakes {
            assert!(!case.acceptable.contains(&mistake.uci));
        }
    }
}

#[cfg(feature = "uci")]
#[tokio::test]
#[ignore = "requires STOCKFISH_PATH pointing to Stockfish 19"]
async fn stockfish_coaching_quality_corpus() -> Result<(), Box<dyn std::error::Error>> {
    use chess_coach::{
        engine::{Engine, EngineProfile, EvaluationRequest},
        practice::{self, Exercise, Grade, Objective},
        uci::Stockfish,
    };
    let mut engine = Stockfish::spawn(std::env::var("STOCKFISH_PATH")?).await?;
    let mut incorrect_grades = Vec::new();
    let mut missed_themes = Vec::new();
    let mut grade_changes = 0;
    let mut answers = 0;
    for case in corpus() {
        let position = position(&case);
        let mut shallow_grades = Vec::new();
        for nodes in [100_000, 5_000_000] {
            let mut request = EvaluationRequest {
                position: position.clone(),
                profile: EngineProfile {
                    engine_name: "Stockfish 19".into(),
                    threads: 1,
                    hash_mb: 64,
                    nodes,
                    timeout_ms: 30_000,
                },
                root_move: None,
            };
            let best = engine.evaluate(&request).await?;
            let exercise = Exercise {
                position: position.clone(),
                best: best.clone(),
                acceptable_loss: 0.02,
                minimum_loss: Some(0.05),
                opening_threshold: None,
                confidence: Default::default(),
                objective: Objective::BestMove,
            };
            for (index, (uci, acceptable, theme)) in case
                .acceptable
                .iter()
                .map(|uci| (uci, true, None))
                .chain(case.mistakes.iter().map(|m| (&m.uci, false, m.theme)))
                .enumerate()
            {
                request.root_move = Some(uci.clone());
                let played = if *uci == best.best_move {
                    best.clone()
                } else {
                    engine.evaluate(&request).await?
                };
                let feedback = practice::grade(&exercise, uci, played, false)?;
                if nodes == 100_000 {
                    shallow_grades.push(feedback.grade);
                    continue;
                }
                answers += 1;
                grade_changes += usize::from(shallow_grades[index] != feedback.grade);
                let accepted = matches!(feedback.grade, Grade::Correct | Grade::Good);
                if accepted != acceptable || feedback.grade == Grade::Inconclusive {
                    incorrect_grades.push(format!(
                        "{} {uci}: {:?}, best={:?}, played={:?}",
                        case.id, feedback.grade, best.score, feedback.played.score
                    ));
                }
                if let Some(theme) = theme
                    && !feedback.teaching.as_ref().unwrap().themes.contains(&theme)
                {
                    missed_themes.push(format!("{} {uci}: {theme:?}", case.id));
                }
                eprintln!(
                    "{} {uci}: {:?}, loss={:.2}pp, best={:?}, played={:?}",
                    case.id,
                    feedback.grade,
                    feedback.loss * 100.0,
                    best.score,
                    feedback.played.score
                );
            }
        }
    }
    eprintln!(
        "{answers} answers; {} incorrect grades; {} missed themes; {grade_changes} shallow/deep grade changes",
        incorrect_grades.len(),
        missed_themes.len()
    );
    assert!(incorrect_grades.is_empty(), "{}", incorrect_grades.join("\n"));
    assert!(missed_themes.is_empty(), "{}", missed_themes.join("\n"));
    Ok(())
}
