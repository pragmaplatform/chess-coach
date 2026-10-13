use chess_coach::{
    analysis::{Observation, Severity},
    engine::{EngineProfile, Evaluation, EvaluationConfidence, EvaluationSource, Score},
    game::{Color, Game, decisions},
    ranking,
};

#[test]
fn weaknesses_rank_single_and_repeated_mistakes_without_counting_revisits() {
    let mut rows = vec![
        observation("a", "a3", 0.2),
        observation("b", "a3", 0.3),
        observation("a", "a3 e5 h3", 0.4),
        observation("b", "a3 e5 h3", 0.1),
        observation("c", "d4 d5 c4", 0.9),
    ];
    let mut repeated_position = rows[0].clone();
    repeated_position.decision.ply = 9;
    repeated_position.loss = 0.8;
    rows.push(repeated_position);

    // Include a successfully analyzed game with no mistakes in the denominator.
    let report = ranking::aggregate(&rows, 4);
    assert_eq!(report.weaknesses.len(), 3);
    assert_eq!(report.weaknesses[0].occurrences, 1);
    assert!((report.weaknesses[0].priority - 0.225).abs() < 1e-12);

    rows.reverse();
    assert_eq!(
        serde_json::to_value(ranking::aggregate(&rows, 4)).unwrap(),
        serde_json::to_value(report).unwrap()
    );
}

#[test]
fn weaknesses_exclude_inconclusive_and_correct_moves() {
    let first = observation("a", "a3", 0.2);
    let mut revisited = first.clone();
    revisited.decision.ply = 5;
    let mut uncertain = observation("b", "a3", 0.3);
    uncertain.confidence = EvaluationConfidence::Inconclusive;
    let different_move = observation("c", "h3", 0.4);
    let mut correct = observation("d", "a3", 0.0);
    correct.mistake = false;
    let rows = [first, revisited, uncertain, different_move, correct];
    let report = ranking::aggregate(&rows, 4);
    assert_eq!(report.weaknesses.len(), 2);
    assert!(report.weaknesses.iter().all(|weakness| weakness.occurrences == 1));
    assert!(ranking::aggregate(&[], 0).weaknesses.is_empty());
}

#[test]
fn legacy_report_remains_readable_without_returning_gain_estimates() {
    let report = ranking::aggregate(&[observation("a", "a3", 0.2), observation("b", "a3", 0.2)], 2);
    let expected = serde_json::to_value(report).unwrap();
    let mut legacy = expected.clone();
    for (key, value) in [
        ("modeled_gain", serde_json::json!(0.1)),
        ("estimated_elo", serde_json::json!(70.4)),
        ("elo_scenario", serde_json::json!("halve_recurring_mistakes_v1")),
        ("recurring_weaknesses", serde_json::json!(1)),
        ("contributing_games", serde_json::json!(2)),
    ] {
        legacy[key] = value;
        assert!(expected.get(key).is_none());
    }
    let restored: ranking::Report = serde_json::from_value(legacy).unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), expected);
}

fn observation(game_id: &str, moves: &str, loss: f64) -> Observation {
    let decision = decisions(
        &Game {
            id: game_id.into(),
            moves_san: moves.into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .pop()
    .unwrap();
    let evaluation = Evaluation {
        source: EvaluationSource::Stockfish,
        score: Score::Centipawns(0),
        best_move: decision.played_uci.clone(),
        line: vec![decision.played_uci.clone()],
        depth: 20,
        nodes: 1_000_000,
    };
    Observation {
        decision,
        best: evaluation.clone(),
        played: evaluation,
        profile: EngineProfile {
            engine_name: "fixture".into(),
            threads: 1,
            hash_mb: 16,
            nodes: 1_000_000,
            timeout_ms: 1000,
        },
        loss,
        confidence: EvaluationConfidence::Confirmed,
        teaching: None,
        severity: Severity::from_loss(loss),
        mistake: true,
        cloud_evaluations: None,
    }
}
