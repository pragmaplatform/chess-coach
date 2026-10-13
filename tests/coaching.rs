use std::collections::VecDeque;

use chess_coach::{
    Error,
    analysis::{AnalysisPolicy, OpeningThreshold, Severity, analyze_game},
    engine::{Engine, EngineProfile, Evaluation, EvaluationConfidence, EvaluationRequest, MoveEvaluation, Score},
    game::{Color, Game, decisions},
    practice::{self, Exercise, Grade, Objective},
    ranking,
    review::{self, ReviewState},
    teaching::Theme,
};

fn profile() -> EngineProfile {
    EngineProfile {
        engine_name: "fixture".into(),
        threads: 1,
        hash_mb: 16,
        nodes: 100,
        timeout_ms: 1000,
    }
}

fn policy() -> AnalysisPolicy {
    AnalysisPolicy {
        version: "test".into(),
        full_moves: 15,
        minimum_loss: 0.05,
        opening_threshold: None,
        acceptable_loss: 0.02,
        engine: profile(),
        confirmation_nodes: 1000,
        verification_nodes: Some(5000),
        cloud_min_depth: None,
    }
}

fn evaluation(uci: &str, score: i32) -> Evaluation {
    Evaluation {
        source: chess_coach::engine::EvaluationSource::Stockfish,
        score: Score::Centipawns(score),
        best_move: uci.into(),
        line: vec![uci.into()],
        depth: 10,
        nodes: 100,
    }
}

#[test]
fn review_positions_preserve_history_and_handle_terminal_boards() {
    use chess_coach::{
        engine::terminal_score,
        game::{board, position_after},
    };
    let start = decisions(
        &Game {
            id: "review".into(),
            moves_san: "e4 e5".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .remove(0)
    .position;
    let moves = |text: &str| text.split_whitespace().map(String::from).collect::<Vec<_>>();
    let after = position_after(&start, &moves("e2e4 e7e5 g1f3")).unwrap();
    assert_eq!(after.history_uci, moves("e2e4 e7e5 g1f3"));
    assert_eq!(
        after.fen,
        "rnbqkbnr/pppp1ppp/8/4p3/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2"
    );
    assert!(board(&after).is_ok());
    assert_eq!(terminal_score(&after).unwrap(), None);
    assert!(position_after(&start, &moves("e2e5")).is_err());
    let mate = position_after(&start, &moves("f2f3 e7e5 g2g4 d8h4")).unwrap();
    assert_eq!(terminal_score(&mate).unwrap(), Some(Score::Mate(0)));
    let draw = position_after(&start, &moves(&"g1f3 g8f6 f3g1 f6g8 ".repeat(25))).unwrap();
    assert_eq!(terminal_score(&draw).unwrap(), Some(Score::Centipawns(0)));
}

#[derive(Default)]
struct FixtureEngine {
    requests: Vec<EvaluationRequest>,
}
#[async_trait::async_trait]
impl Engine for FixtureEngine {
    async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
        self.requests.push(request.clone());
        let best = if request.position.history_uci.len().is_multiple_of(2) {
            "e2e4"
        } else {
            "e7e5"
        };
        Ok(evaluation(
            request.root_move.as_deref().unwrap_or(best),
            if request.root_move.is_some() { -100 } else { 100 },
        ))
    }
}

#[tokio::test]
async fn confirms_candidate_mistakes_and_keeps_side_to_move_perspective() {
    let policy = policy();
    for (moves, color) in [("a3", Color::White), ("e4 a6", Color::Black)] {
        let mut engine = FixtureEngine::default();
        let rows = analyze_game(
            &Game {
                id: "test".into(),
                moves_san: moves.into(),
                player_color: color,
            },
            &policy,
            &mut engine,
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].mistake);
        assert_eq!(rows[0].severity, Some(Severity::Blunder));
        assert!((rows[0].loss - 0.181956).abs() < 0.00001);
        assert_eq!(rows[0].profile.nodes, 1000);
        assert_eq!(
            engine.requests.iter().map(|r| r.profile.nodes).collect::<Vec<_>>(),
            [100, 100, 1000, 1000]
        );
        assert_eq!(
            engine.requests[1].root_move.as_deref(),
            Some(rows[0].decision.played_uci.as_str())
        );
    }
}

struct ChangingEngine {
    scores: VecDeque<i32>,
    requests: Vec<EvaluationRequest>,
}

struct CloudEngine {
    cloud: Vec<Evaluation>,
    local: ChangingEngine,
}

#[async_trait::async_trait]
impl Engine for CloudEngine {
    async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
        self.local.evaluate(request).await
    }

    async fn cloud_evaluations(&mut self, _: &EvaluationRequest, _: u16) -> Result<Option<Vec<Evaluation>>, Error> {
        Ok(Some(self.cloud.clone()))
    }
}

#[tokio::test]
async fn cloud_comparisons_skip_local_work_or_confirm_both_moves_together() {
    use chess_coach::engine::EvaluationSource;
    for (cloud_cp, depth, local_scores, expected_nodes, expected_source) in [
        (Some(-75), 22, vec![], vec![], EvaluationSource::LichessCloud),
        (Some(-5), 22, vec![], vec![], EvaluationSource::LichessCloud),
        (
            None,
            22,
            vec![0, -75, 0, -75],
            vec![100, 100, 1000, 1000],
            EvaluationSource::Stockfish,
        ),
        (
            Some(-75),
            21,
            vec![0, -75, 0, -75],
            vec![100, 100, 1000, 1000],
            EvaluationSource::Stockfish,
        ),
        (
            Some(-55),
            22,
            vec![0, -75],
            vec![1000, 1000],
            EvaluationSource::Stockfish,
        ),
        (
            Some(10),
            22,
            vec![0, -55, 0, -20],
            vec![1000, 1000, 5000, 5000],
            EvaluationSource::Stockfish,
        ),
    ] {
        let mut cloud = vec![evaluation("e2e4", 0)];
        if let Some(cp) = cloud_cp {
            cloud.push(evaluation("a2a3", cp));
        }
        for evaluation in &mut cloud {
            evaluation.source = EvaluationSource::LichessCloud;
            evaluation.depth = depth;
        }
        let mut engine = CloudEngine {
            cloud,
            local: ChangingEngine {
                scores: local_scores.into(),
                requests: Vec::new(),
            },
        };
        let mut policy = policy();
        policy.cloud_min_depth = Some(22);
        let observations = analyze_game(
            &Game {
                id: "cloud".into(),
                moves_san: "a3".into(),
                player_color: Color::White,
            },
            &policy,
            &mut engine,
        )
        .await
        .unwrap();
        assert_eq!(observations[0].best.source, expected_source);
        assert_eq!(observations[0].played.source, expected_source);
        assert_eq!(
            engine
                .local
                .requests
                .iter()
                .map(|request| request.profile.nodes)
                .collect::<Vec<_>>(),
            expected_nodes
        );
        assert!(engine.local.scores.is_empty());
        if expected_source == EvaluationSource::LichessCloud {
            assert_eq!(observations[0].profile, policy.confirmation_profile());
            assert_eq!(observations[0].mistake, cloud_cp == Some(-75));
        }
    }
}

#[tokio::test]
async fn opening_preferences_are_excluded_for_both_colors_through_move_five() {
    struct OpeningEngine {
        decisions: Vec<chess_coach::game::Decision>,
        played_cp: i32,
    }
    #[async_trait::async_trait]
    impl Engine for OpeningEngine {
        async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
            if let Some(uci) = &request.root_move {
                return Ok(evaluation(uci, self.played_cp));
            }
            let decision = self.decisions.iter().find(|d| d.position == request.position).unwrap();
            let board = chess_coach::game::board(&request.position)?;
            let mut alternative = None;
            board.generate_moves(|moves| {
                for mv in moves {
                    let uci = cozy_chess::util::display_uci_move(&board, mv).to_string();
                    if uci != decision.played_uci {
                        alternative = Some(uci);
                        return true;
                    }
                }
                false
            });
            Ok(evaluation(&alternative.unwrap(), 0))
        }
    }
    for played_cp in [-86, -500] {
        for color in [Color::White, Color::Black] {
            let game = Game {
                id: "opening-cutoff".into(),
                moves_san: "e4 e5 Nf3 Nc6 Bb5 a6 Ba4 Nf6 O-O Be7 Re1 b5".into(),
                player_color: color,
            };
            let mut engine = OpeningEngine {
                decisions: decisions(&game, 15).unwrap(),
                played_cp,
            };
            let mut policy = policy();
            policy.opening_threshold = Some(Default::default());
            let observations = analyze_game(&game, &policy, &mut engine).await.unwrap();
            for observation in &observations {
                assert!(observation.loss > 0.05);
                if played_cp == -500 {
                    assert!(observation.loss > 0.15);
                }
                assert_eq!(observation.mistake, observation.decision.ply > 10);
                let exercise = Exercise {
                    position: observation.decision.position.clone(),
                    best: observation.best.clone(),
                    acceptable_loss: policy.acceptable_loss,
                    minimum_loss: Some(policy.minimum_loss),
                    opening_threshold: policy.opening_threshold,
                    confidence: observation.confidence,
                    objective: Objective::BestMove,
                };
                let feedback = practice::grade(
                    &exercise,
                    &observation.decision.played_uci,
                    observation.played.clone(),
                    false,
                )
                .unwrap();
                assert_eq!(
                    feedback.grade,
                    if observation.loss >= exercise.minimum_loss().unwrap() {
                        Grade::Incorrect
                    } else {
                        Grade::Good
                    }
                );
            }
            assert_eq!(ranking::aggregate(&observations, 1).weaknesses.len(), 1);
        }
    }
}

#[tokio::test]
async fn early_opening_threshold_drives_local_and_cloud_verification() {
    for cloud in [false, true] {
        // About 10 points, so both paths must verify at the final budget.
        let mut engine = CloudEngine {
            cloud: [evaluation("e2e4", 0), evaluation("a2a3", -110)]
                .into_iter()
                .map(|mut e| {
                    e.source = chess_coach::engine::EvaluationSource::LichessCloud;
                    e.depth = 22;
                    e
                })
                .collect(),
            local: ChangingEngine {
                scores: if cloud {
                    vec![0, -110, 0, -115]
                } else {
                    vec![0, -110, 0, -110, 0, -115]
                }
                .into(),
                requests: Vec::new(),
            },
        };
        let mut policy = policy();
        policy.opening_threshold = Some(Default::default());
        policy.cloud_min_depth = cloud.then_some(22);
        let observations = analyze_game(
            &Game {
                id: "borderline-opening".into(),
                moves_san: "a3".into(),
                player_color: Color::White,
            },
            &policy,
            &mut engine,
        )
        .await
        .unwrap();
        assert!(engine.local.scores.is_empty());
        assert_eq!(observations[0].profile.nodes, 5000);
        assert!(!observations[0].mistake);
        assert_eq!(observations[0].confidence, EvaluationConfidence::Confirmed);
    }
}

#[tokio::test]
async fn early_bf4_is_excluded_while_legacy_policies_keep_their_cutoff() {
    struct Bf4Engine;
    #[async_trait::async_trait]
    impl Engine for Bf4Engine {
        async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
            if request.position.history_uci.is_empty() {
                return Ok(evaluation("d2d4", 0));
            }
            if request.root_move.is_some() {
                let mut played = evaluation("c1f4", -64);
                played.line = ["c1f4", "c5d4", "c2c3", "d4c3", "b1c3"].map(String::from).to_vec();
                Ok(played)
            } else {
                Ok(evaluation("d4d5", 62))
            }
        }
    }
    let game = Game {
        id: "early-bf4".into(),
        moves_san: "d4 c5 Bf4".into(),
        player_color: Color::White,
    };
    let legacy: OpeningThreshold = serde_json::from_str(r#"{"full_moves":5,"minimum_loss":0.10}"#).unwrap();
    assert!(!legacy.mates_only);
    for (threshold, eligible) in [
        (Some(OpeningThreshold::default()), false),
        (Some(legacy), true),
        (None, true),
    ] {
        let mut policy = policy();
        policy.opening_threshold = threshold;
        // Persisting and reloading must keep the policy's selection behavior.
        let policy = serde_json::from_value(serde_json::to_value(policy).unwrap()).unwrap();
        let observations = analyze_game(&game, &policy, &mut Bf4Engine).await.unwrap();
        let bf4 = &observations[1];
        assert!((bf4.loss - 0.1154).abs() < 0.0001);
        assert_eq!(bf4.confidence, EvaluationConfidence::Confirmed);
        assert!(bf4.teaching.as_ref().unwrap().themes.contains(&Theme::MaterialLoss));
        assert_eq!(bf4.mistake, eligible);
        assert_eq!(
            ranking::aggregate(&observations, 1).weaknesses.len(),
            usize::from(eligible)
        );
    }
}

#[tokio::test]
async fn early_allowed_mate_remains_eligible() {
    struct MateEngine;
    #[async_trait::async_trait]
    impl Engine for MateEngine {
        async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
            if request.position.history_uci.is_empty() {
                return Ok(evaluation("f2f3", 0));
            }
            let mut value = evaluation(request.root_move.as_deref().unwrap_or("e2e4"), 0);
            if request.root_move.is_some() {
                value.score = Score::Mate(-1);
                value.line.push("d8h4".into());
            }
            Ok(value)
        }
    }
    let mut policy = policy();
    policy.opening_threshold = Some(Default::default());
    let observations = analyze_game(
        &Game {
            id: "allowed-mate".into(),
            moves_san: "f3 e5 g4".into(),
            player_color: Color::White,
        },
        &policy,
        &mut MateEngine,
    )
    .await
    .unwrap();
    let allowed = &observations[1];
    assert!(allowed.mistake);
    assert_eq!(allowed.confidence, EvaluationConfidence::Confirmed);
    let report = ranking::aggregate(&observations, 1);
    assert_eq!(report.weaknesses.len(), 1);
    assert_eq!(
        Objective::from_themes(&report.weaknesses[0].themes),
        Objective::AvoidMate
    );
}

#[test]
fn opening_grading_keeps_legacy_attempts_and_recalculates_follow_up_thresholds() {
    let mut exercise = exercise_at(
        "e4 e5 Nf3 Nc6 Bb5 a6 Ba4 Nf6 O-O Be7",
        Color::Black,
        evaluation("b7b5", 0),
    );
    let played = evaluation("f8e7", -86);
    assert_eq!(
        practice::grade(&exercise, "f8e7", played.clone(), false).unwrap().grade,
        Grade::Incorrect
    );
    exercise.opening_threshold = Some(Default::default());
    assert_eq!(
        practice::grade(&exercise, "f8e7", played, false).unwrap().grade,
        Grade::Good
    );
    exercise.position = chess_coach::game::position_after(&exercise.position, &["f8e7".into(), "f1e1".into()]).unwrap();
    assert_eq!(exercise.minimum_loss(), Some(0.05));

    let mut exercise = exercise_at("f3 c6", Color::Black, evaluation("e7e5", 71));
    exercise.opening_threshold = Some(Default::default());
    assert_eq!(
        practice::grade(&exercise, "c7c6", evaluation("c7c6", -15), false)
            .unwrap()
            .grade,
        Grade::Good
    );
    let mut json = serde_json::to_value(&exercise).unwrap();
    json.as_object_mut().unwrap().remove("opening_threshold");
    let legacy: Exercise = serde_json::from_value(json).unwrap();
    assert_eq!(
        practice::grade(&legacy, "c7c6", evaluation("c7c6", -15), false)
            .unwrap()
            .grade,
        Grade::Incorrect
    );
}

#[test]
fn cloud_positions_preserve_draw_history_and_normalize_castling() {
    use chess_coach::game::{cloud_fen, normalize_line};
    let rows = decisions(
        &Game {
            id: "history".into(),
            moves_san: "Nf3 Nf6 g3".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap();
    assert!(cloud_fen(&rows[0].position).unwrap().is_some());
    assert!(cloud_fen(&rows[1].position).unwrap().is_none());
    let moves = "e2e4 e7e5 g1f3 b8c6 f1b5 a7a6 e1h1"
        .split_whitespace()
        .map(String::from)
        .collect::<Vec<_>>();
    let normalized = normalize_line(&rows[0].position, &moves).unwrap();
    assert_eq!(normalized.last().unwrap(), "e1g1");
    assert!(normalize_line(&rows[0].position, &["e2e5".into()]).is_err());
    let exercise = Exercise {
        position: rows[0].position.clone(),
        best: evaluation("e2e4", 0),
        acceptable_loss: 0.02,
        minimum_loss: Some(0.05),
        opening_threshold: None,
        confidence: Default::default(),
        objective: Default::default(),
    };
    let mut played = evaluation("d2d4", 0);
    played.source = chess_coach::engine::EvaluationSource::LichessCloud;
    assert!(practice::grade(&exercise, "d2d4", played, false).is_err());
}

#[async_trait::async_trait]
impl Engine for ChangingEngine {
    async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
        self.requests.push(request.clone());
        Ok(evaluation(
            request.root_move.as_deref().unwrap_or("e2e4"),
            self.scores.pop_front().expect("Unexpected search"),
        ))
    }
}

#[tokio::test]
async fn final_search_decides_severity_and_keeps_five_point_inaccuracies_in_practice() {
    for (scores, severity) in [
        ([0, -70, 0, -56, 0, -65], Some(Severity::Inaccuracy)),
        ([0, -70, 0, -56, 0, -20], None),
        ([0, -70, 0, -45, 0, -65], Some(Severity::Inaccuracy)),
        // Large changes at the final budget remain inconclusive instead of becoming practice cards.
        ([0, -70, 0, -56, 0, -120], None),
        ([0, -70, 0, -56, 0, -180], None),
        ([0, 10, 0, -56, 0, -65], Some(Severity::Inaccuracy)),
        ([0, -70, 0, 10, 0, -65], None),
    ] {
        let mut engine = ChangingEngine {
            scores: scores.into(),
            requests: Vec::new(),
        };
        let observations = analyze_game(
            &Game {
                id: "changing-evaluation".into(),
                moves_san: "a3".into(),
                player_color: Color::White,
            },
            &policy(),
            &mut engine,
        )
        .await
        .unwrap();
        let observation = &observations[0];
        assert_eq!(observation.severity, severity, "scores: {scores:?}");
        assert_eq!(observation.mistake, severity.is_some());
        assert_eq!(observation.profile.nodes, 5000);
        assert_eq!(
            engine.requests.iter().map(|r| r.profile.nodes).collect::<Vec<_>>(),
            [100, 100, 1000, 1000, 5000, 5000]
        );
        let report = ranking::aggregate(&observations, 1);
        assert_eq!(report.weaknesses.len(), usize::from(severity.is_some()));
        if let Some(weakness) = report.weaknesses.first() {
            assert_eq!(weakness.severity, severity);
            assert_eq!(weakness.example.profile.nodes, 5000);
        }
    }
}

#[tokio::test]
async fn clear_mistakes_keep_the_existing_confirmation_budget() {
    let mut engine = ChangingEngine {
        scores: [0, -70, 0, -70].into(),
        requests: Vec::new(),
    };
    let observations = analyze_game(
        &Game {
            id: "clear-inaccuracy".into(),
            moves_san: "a3".into(),
            player_color: Color::White,
        },
        &policy(),
        &mut engine,
    )
    .await
    .unwrap();
    assert!(observations[0].mistake);
    assert_eq!(observations[0].severity, Some(Severity::Inaccuracy));
    assert_eq!(observations[0].profile.nodes, 1000);
    assert_eq!(engine.requests.len(), 4);
}

#[tokio::test]
async fn stops_searching_when_the_move_is_best_or_the_candidate_disappears() {
    for (moves, scores, nodes) in [
        ("e4", vec![0], 100),
        ("a3", vec![0, -20], 100),
        ("a3", vec![0, -70, 0, -20, 0, -20], 5000),
    ] {
        let mut engine = ChangingEngine {
            scores: scores.into(),
            requests: Vec::new(),
        };
        let observations = analyze_game(
            &Game {
                id: "no-mistake".into(),
                moves_san: moves.into(),
                player_color: Color::White,
            },
            &policy(),
            &mut engine,
        )
        .await
        .unwrap();
        assert!(!observations[0].mistake);
        assert_eq!(observations[0].severity, None);
        assert_eq!(observations[0].profile.nodes, nodes);
        assert!(engine.scores.is_empty());
    }
}

#[test]
fn severity_thresholds_are_inclusive() {
    for (loss, severity) in [
        (0.0, None),
        (0.05_f64.next_down(), None),
        (0.05, Some(Severity::Inaccuracy)),
        (0.10_f64.next_down(), Some(Severity::Inaccuracy)),
        (0.10, Some(Severity::Mistake)),
        (0.15_f64.next_down(), Some(Severity::Mistake)),
        (0.15, Some(Severity::Blunder)),
        (1.0, Some(Severity::Blunder)),
    ] {
        assert_eq!(Severity::from_loss(loss), severity);
    }
}

#[tokio::test]
async fn older_policies_and_saved_results_remain_readable() {
    let mut json = serde_json::to_value(policy()).unwrap();
    json.as_object_mut().unwrap().remove("verification_nodes");
    json.as_object_mut().unwrap().remove("cloud_min_depth");
    json.as_object_mut().unwrap().remove("opening_threshold");
    let old: AnalysisPolicy = serde_json::from_value(json).unwrap();
    old.validate().unwrap();
    assert!(old.verification_profile().is_none());
    assert!(old.cloud_min_depth.is_none());
    assert!(old.opening_threshold.is_none());
    let mut engine = FixtureEngine::default();
    let observations = analyze_game(
        &Game {
            id: "legacy".into(),
            moves_san: "a3".into(),
            player_color: Color::White,
        },
        &old,
        &mut engine,
    )
    .await
    .unwrap();
    assert_eq!(observations[0].profile.nodes, 1000);
    assert_eq!(engine.requests.len(), 4);
    let mut json = serde_json::to_value(&ranking::aggregate(&observations, 1).weaknesses[0]).unwrap();
    json.as_object_mut().unwrap().remove("severity");
    json["example"].as_object_mut().unwrap().remove("severity");
    json["example"].as_object_mut().unwrap().remove("confidence");
    json["example"].as_object_mut().unwrap().remove("teaching");
    json.as_object_mut().unwrap().remove("themes");
    json["example"].as_object_mut().unwrap().remove("cloud_evaluations");
    json["example"]["best"].as_object_mut().unwrap().remove("source");
    json["example"]["played"].as_object_mut().unwrap().remove("source");
    let old: ranking::Weakness = serde_json::from_value(json).unwrap();
    assert!(old.severity.is_none());
    assert!(old.example.severity.is_none());
    assert_eq!(old.example.confidence, EvaluationConfidence::Unverified);
    assert!(old.example.teaching.is_none());
    assert!(old.themes.is_empty());
    assert!(old.example.mistake);
    assert_eq!(
        old.example.best.source,
        chess_coach::engine::EvaluationSource::Stockfish
    );
    assert!(old.example.cloud_evaluations.is_none());

    let mut invalid = policy();
    invalid.verification_nodes = Some(invalid.confirmation_nodes);
    assert!(invalid.validate().is_err());
    invalid.verification_nodes = Some(invalid.engine.nodes);
    assert!(invalid.validate().is_err());
}

#[tokio::test]
async fn aggregates_frequency_across_distinct_games() {
    let policy = policy();
    let mut engine = FixtureEngine::default();
    let mut rows = analyze_game(
        &Game {
            id: "a".into(),
            moves_san: "a3".into(),
            player_color: Color::White,
        },
        &policy,
        &mut engine,
    )
    .await
    .unwrap();
    let mut repeat = rows[0].clone();
    repeat.decision.ply = 5;
    rows.push(repeat); // Revisiting a position in one game is one occurrence.
    let mut other_game = rows[0].clone();
    other_game.decision.game_id = "b".into();
    rows.push(other_game);
    let mut good_game = rows[0].clone();
    good_game.decision.game_id = "c".into();
    good_game.mistake = false;
    good_game.loss = 0.0;
    rows.push(good_game);
    let report = ranking::aggregate(&rows, 10);
    assert_eq!(report.weaknesses.len(), 1);
    let weakness = &report.weaknesses[0];
    assert_eq!(weakness.occurrences, 2);
    assert_eq!(weakness.reached_games, 3);
    assert!((weakness.priority - rows[0].loss * 0.2).abs() < 1e-12);
}

#[test]
fn practice_accepts_good_alternatives_and_rejects_illegal_or_mismatched_moves() {
    let decision = decisions(
        &Game {
            id: "game".into(),
            moves_san: "a3".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .remove(0);
    let exercise = Exercise {
        position: decision.position,
        best: evaluation("e2e4", 30),
        acceptable_loss: 0.02,
        minimum_loss: Some(0.05),
        opening_threshold: None,
        confidence: Default::default(),
        objective: Default::default(),
    };
    assert_eq!(
        practice::grade(&exercise, "d2d4", evaluation("d2d4", 0), false)
            .unwrap()
            .grade,
        Grade::Good
    );
    assert_eq!(
        practice::grade(&exercise, "d2d4", evaluation("d2d4", 20), false)
            .unwrap()
            .grade,
        Grade::Correct
    );
    assert_eq!(
        practice::grade(&exercise, "a2a3", evaluation("a2a3", -100), false)
            .unwrap()
            .grade,
        Grade::Incorrect
    );
    assert_eq!(
        practice::grade(&exercise, "e2e4", evaluation("e2e4", 30), true)
            .unwrap()
            .grade,
        Grade::Assisted
    );
    assert!(practice::grade(&exercise, "e2e5", evaluation("e2e5", 30), false).is_err());
    assert!(practice::grade(&exercise, "d2d4", evaluation("e2e4", 30), false).is_err());
}

#[test]
fn grading_preserves_threshold_boundaries_and_older_exercises() {
    let position = decisions(
        &Game {
            id: "grading".into(),
            moves_san: "a3".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .remove(0)
    .position;
    let mut exercise = Exercise {
        position,
        best: evaluation("e2e4", 0),
        acceptable_loss: 0.02,
        minimum_loss: Some(0.05),
        opening_threshold: None,
        confidence: Default::default(),
        objective: Default::default(),
    };
    for (cp, grade) in [
        (10, Grade::Correct),
        (-21, Grade::Correct),
        (-22, Grade::Good),
        (-54, Grade::Good),
        (-55, Grade::Incorrect),
    ] {
        let played = evaluation("d2d4", cp);
        assert_eq!(
            practice::grade(&exercise, "d2d4", played.clone(), false).unwrap().grade,
            grade
        );
        assert_eq!(
            practice::grade(&exercise, "d2d4", played, true).unwrap().grade,
            if grade == Grade::Inconclusive {
                Grade::Inconclusive
            } else {
                Grade::Assisted
            }
        );
    }
    let mut json = serde_json::to_value(&exercise).unwrap();
    json.as_object_mut().unwrap().remove("minimum_loss");
    json.as_object_mut().unwrap().remove("confidence");
    json.as_object_mut().unwrap().remove("objective");
    let old: Exercise = serde_json::from_value(json).unwrap();
    assert_eq!(old.objective, Objective::BestMove);
    assert_eq!(old.confidence, EvaluationConfidence::Unverified);
    assert_eq!(
        practice::grade(&old, "d2d4", evaluation("d2d4", -30), false)
            .unwrap()
            .grade,
        Grade::Incorrect
    );

    let played = evaluation("d2d4", -55);
    let exact_loss = chess_coach::engine::loss(exercise.best.score, played.score);
    exercise.minimum_loss = Some(exact_loss);
    assert_eq!(
        practice::grade(&exercise, "d2d4", played.clone(), false).unwrap().grade,
        Grade::Incorrect
    );
    exercise.acceptable_loss = exact_loss;
    exercise.minimum_loss = Some(0.1);
    assert_eq!(
        practice::grade(&exercise, "d2d4", played, false).unwrap().grade,
        Grade::Correct
    );
}

#[test]
fn cache_identity_includes_history_profile_and_restricted_move() {
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
        .position
    };
    let a = EvaluationRequest {
        position: get("Nf3 Nf6 g3 g6 Bg2"),
        profile: profile(),
        root_move: None,
    };
    let mut b = a.clone();
    b.position = get("g3 g6 Nf3 Nf6 Bg2");
    assert_eq!(a.position.key, b.position.key);
    assert_ne!(
        chess_coach::fingerprint(&a).unwrap(),
        chess_coach::fingerprint(&b).unwrap()
    );
    b = a.clone();
    b.profile.nodes += 1;
    assert_ne!(
        chess_coach::fingerprint(&a).unwrap(),
        chess_coach::fingerprint(&b).unwrap()
    );
    b = a.clone();
    b.root_move = Some("f1g2".into());
    assert_ne!(
        chess_coach::fingerprint(&a).unwrap(),
        chess_coach::fingerprint(&b).unwrap()
    );
}

#[tokio::test]
async fn unsettled_searches_do_not_become_lessons_or_successful_reviews() {
    for scores in [[0, 100, 0, 100, 0, 100], [0, -70, 0, -300, 0, -700]] {
        let mut engine = ChangingEngine {
            scores: scores.into(),
            requests: Vec::new(),
        };
        let observation = analyze_game(
            &Game {
                id: "unsettled".into(),
                moves_san: "a3".into(),
                player_color: Color::White,
            },
            &policy(),
            &mut engine,
        )
        .await
        .unwrap()
        .remove(0);
        assert_eq!(engine.requests.len(), 6);
        assert_eq!(observation.confidence, EvaluationConfidence::Inconclusive);
        assert!(!observation.mistake);
        assert_eq!(observation.severity, None);
        assert!(observation.teaching.as_ref().unwrap().themes.is_empty());
        assert!(
            ranking::aggregate(std::slice::from_ref(&observation), 1)
                .weaknesses
                .is_empty()
        );
        let exercise = Exercise {
            position: observation.decision.position,
            best: observation.best,
            acceptable_loss: 0.02,
            minimum_loss: Some(0.05),
            opening_threshold: None,
            confidence: observation.confidence,
            objective: Objective::BestMove,
        };
        for assisted in [false, true] {
            let feedback = practice::grade(&exercise, "a2a3", observation.played.clone(), assisted).unwrap();
            assert_eq!(feedback.grade, Grade::Inconclusive);
            assert_eq!(feedback.objective_met, None);
            assert!(practice::follow_up(&exercise.position, &feedback).unwrap().is_none());
            let state = ReviewState {
                step: 4,
                reviews: 12,
                lapses: 2,
                due_at: 1234,
            };
            assert_eq!(
                serde_json::to_value(review::schedule(&state, feedback.grade, 5000)).unwrap(),
                serde_json::to_value(state).unwrap()
            );
        }
    }
}

#[tokio::test]
async fn large_search_changes_get_verified_even_far_from_the_practice_threshold() {
    let mut engine = ChangingEngine {
        scores: [0, -70, 0, -300, 0, -305].into(),
        requests: Vec::new(),
    };
    let observation = analyze_game(
        &Game {
            id: "settled".into(),
            moves_san: "a3".into(),
            player_color: Color::White,
        },
        &policy(),
        &mut engine,
    )
    .await
    .unwrap()
    .remove(0);
    assert_eq!(engine.requests.len(), 6);
    assert_eq!(observation.confidence, EvaluationConfidence::Confirmed);
    assert!(observation.mistake);
    assert_eq!(observation.severity, Some(Severity::Blunder));
}

fn exercise_at(moves: &str, color: Color, best: Evaluation) -> Exercise {
    Exercise {
        position: decisions(
            &Game {
                id: "lesson".into(),
                moves_san: moves.into(),
                player_color: color,
            },
            15,
        )
        .unwrap()
        .pop()
        .unwrap()
        .position,
        best,
        acceptable_loss: 0.02,
        minimum_loss: Some(0.05),
        opening_threshold: None,
        confidence: EvaluationConfidence::Unverified,
        objective: Objective::BestMove,
    }
}

#[test]
fn probability_saturation_does_not_hide_a_changed_mating_outcome() {
    let comparison = |score| {
        let mut value = evaluation("e2e4", 0);
        value.score = score;
        MoveEvaluation::new(value.clone(), value)
    };
    for (mate, cp) in [(Score::Mate(3), 1100), (Score::Mate(-3), -1100)] {
        let mate = comparison(mate);
        let cp = comparison(Score::Centipawns(cp));
        assert_eq!(
            mate.clone().compared_with(&cp).confidence,
            EvaluationConfidence::Inconclusive
        );
        assert_eq!(cp.compared_with(&mate).confidence, EvaluationConfidence::Inconclusive);
    }
    assert_eq!(
        comparison(Score::Mate(5))
            .compared_with(&comparison(Score::Mate(3)))
            .confidence,
        EvaluationConfidence::Confirmed
    );
}

#[test]
fn missed_mate_is_taught_even_when_probability_loss_is_small() {
    let mut best = evaluation("d8h4", 0);
    best.score = Score::Mate(1);
    let mut exercise = exercise_at("f3 e5 g4 Qh4#", Color::Black, best);
    // Synthetic +11 isolates the saturated probability model; the moves and mate are legal.
    let played = evaluation("b8c6", 1100);
    let feedback = practice::grade(&exercise, "b8c6", played.clone(), false).unwrap();
    assert_eq!(feedback.grade, Grade::Correct);
    assert!(feedback.teaching.as_ref().unwrap().themes.contains(&Theme::MissedMate));
    assert!(feedback.teaching.as_ref().unwrap().explanation.contains("Qh4#"));
    exercise.objective = Objective::FindMate;
    let feedback = practice::grade(&exercise, "b8c6", played, false).unwrap();
    assert_eq!(feedback.grade, Grade::Incorrect);
    assert_eq!(feedback.objective_met, Some(false));
    let solved = practice::grade(&exercise, "d8h4", exercise.best.clone(), false).unwrap();
    assert_eq!(solved.grade, Grade::Correct);
    assert_eq!(solved.objective_met, Some(true));
    assert!(practice::follow_up(&exercise.position, &solved).unwrap().is_none());
}

#[test]
fn obsolete_mating_objectives_are_inconclusive_even_for_the_refreshed_best_move() {
    for (objective, score) in [
        (Objective::FindMate, Score::Centipawns(800)),
        (Objective::AvoidMate, Score::Mate(-3)),
    ] {
        let mut best = evaluation("e2e4", 0);
        best.score = score;
        best.line = ["e2e4", "e7e5", "g1f3"].map(String::from).to_vec();
        let mut exercise = exercise_at("a3", Color::White, best);
        exercise.objective = objective;
        exercise.confidence = EvaluationConfidence::Confirmed;
        for assisted in [false, true] {
            let feedback = practice::grade(&exercise, "e2e4", exercise.best.clone(), assisted).unwrap();
            assert_eq!(feedback.grade, Grade::Inconclusive);
            assert_eq!(feedback.confidence, EvaluationConfidence::Inconclusive);
            assert_eq!(feedback.objective_met, None);
            assert_eq!(feedback.loss, 0.0);
            assert!(feedback.teaching.as_ref().unwrap().themes.is_empty());
            assert!(practice::follow_up(&exercise.position, &feedback).unwrap().is_none());
        }
    }
}

#[tokio::test]
async fn verified_mating_lessons_are_practiced_below_the_probability_threshold() {
    struct MateEngine {
        searches: usize,
    }
    #[async_trait::async_trait]
    impl Engine for MateEngine {
        async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
            self.searches += 1;
            if request.position.history_uci.len() == 1 {
                return Ok(evaluation("e7e5", 0));
            }
            let mut value = evaluation(request.root_move.as_deref().unwrap_or("d8h4"), 1100);
            if request.root_move.is_none() {
                value.score = Score::Mate(1);
            }
            Ok(value)
        }
    }
    let mut engine = MateEngine { searches: 0 };
    let mut policy = policy();
    policy.opening_threshold = Some(Default::default());
    let observations = analyze_game(
        &Game {
            id: "missed-mate".into(),
            moves_san: "f3 e5 g4 Nc6".into(),
            player_color: Color::Black,
        },
        &policy,
        &mut engine,
    )
    .await
    .unwrap();
    assert_eq!(engine.searches, 7);
    let missed = &observations[1];
    assert!(missed.loss < policy.minimum_loss);
    assert!(missed.mistake);
    assert_eq!(missed.confidence, EvaluationConfidence::Confirmed);
    assert_eq!(missed.profile.nodes, 5000);
    assert_eq!(missed.severity, None);
    let report = ranking::aggregate(&observations, 1);
    assert_eq!(report.weaknesses.len(), 1);
    assert_eq!(
        Objective::from_themes(&report.weaknesses[0].themes),
        Objective::FindMate
    );
}

#[test]
fn explanations_use_legal_replies_and_material_from_the_shown_lines() {
    let exercise = exercise_at("e4 e5 Qh5 Nc6 Qxe5+ Nxe5", Color::Black, evaluation("c6e5", 900));
    let mut played = evaluation("d8e7", 0);
    played.line.push("e5e7".into());
    let feedback = practice::grade(&exercise, "d8e7", played, false).unwrap();
    let teaching = feedback.teaching.unwrap();
    assert!(teaching.themes.contains(&Theme::MaterialLoss));
    assert_eq!(teaching.best.material_change, 9);
    assert_eq!(teaching.played.material_change, -9);
    assert_eq!(teaching.played.moves_san, ["Qe7", "Qxe7+"]);
    assert!(teaching.explanation.contains("Qxe7+"));
    let mut illegal = evaluation("d8e7", 0);
    illegal.line.push("a2a5".into());
    assert!(practice::grade(&exercise, "d8e7", illegal, false).is_err());
}

#[test]
fn different_searches_of_the_same_move_do_not_create_comparative_tactical_claims() {
    // The shorter line ends before the queen recapture; neither search recommends a different move.
    for (best_score, played_score) in [
        (Score::Centipawns(0), Score::Centipawns(-5)),
        (Score::Mate(3), Score::Centipawns(1100)),
        (Score::Centipawns(-1100), Score::Mate(-3)),
    ] {
        let mut best = evaluation("d8e7", 0);
        best.score = best_score;
        best.line = ["d8e7", "e5e7", "c6e7"].map(String::from).to_vec();
        let exercise = exercise_at("e4 e5 Qh5 Nc6 Qxe5+ Qe7", Color::Black, best);
        let mut played = evaluation("d8e7", 0);
        played.score = played_score;
        played.line = ["d8e7", "e5e7"].map(String::from).to_vec();
        let feedback = practice::grade(&exercise, "d8e7", played, false).unwrap();
        assert_eq!(feedback.grade, Grade::Correct);
        let teaching = feedback.teaching.unwrap();
        assert_eq!(teaching.best.material_change, 0);
        assert_eq!(teaching.played.material_change, -9);
        assert!(teaching.themes.is_empty());
        assert_eq!(
            teaching.explanation,
            "Qe7 matches the engine's preferred move. Continue the line to see the opponent's defense. The engine's reply to Qe7 is Qxe7+."
        );
    }
}

#[test]
fn follow_ups_continue_the_submitted_alternative_or_demonstrate_the_correction() {
    let mut best = evaluation("e2e4", 100);
    best.line = ["e2e4", "e7e5", "g1f3"].map(String::from).to_vec();
    let exercise = exercise_at("a3", Color::White, best);
    let mut alternative = evaluation("d2d4", 90);
    alternative.line = ["d2d4", "d7d5", "c2c4"].map(String::from).to_vec();
    let good = practice::grade(&exercise, "d2d4", alternative, false).unwrap();
    let follow_up = practice::follow_up(&exercise.position, &good).unwrap().unwrap();
    assert_eq!(follow_up.setup_san, ["d4", "d5"]);
    assert_eq!(follow_up.position.history_uci, ["d2d4", "d7d5"]);
    let wrong = practice::grade(&exercise, "a2a3", evaluation("a2a3", -100), false).unwrap();
    let follow_up = practice::follow_up(&exercise.position, &wrong).unwrap().unwrap();
    assert_eq!(follow_up.setup_san, ["e4", "e5"]);
    assert_eq!(follow_up.position.history_uci, ["e2e4", "e7e5"]);
}

#[tokio::test]
async fn teaching_examples_prefer_confirmation_and_themes_span_distinct_positions() {
    let mut rows = analyze_game(
        &Game {
            id: "z-confirmed".into(),
            moves_san: "a3".into(),
            player_color: Color::White,
        },
        &policy(),
        &mut FixtureEngine::default(),
    )
    .await
    .unwrap();
    let mut unverified = rows[0].clone();
    unverified.decision.game_id = "a-unverified".into();
    unverified.confidence = EvaluationConfidence::Unverified;
    unverified.best.depth = 30;
    unverified.played.depth = 30;
    rows.push(unverified);
    rows.extend(
        analyze_game(
            &Game {
                id: "second-position".into(),
                moves_san: "e4 a6".into(),
                player_color: Color::Black,
            },
            &policy(),
            &mut FixtureEngine::default(),
        )
        .await
        .unwrap(),
    );
    for row in &mut rows {
        row.teaching.as_mut().unwrap().themes = vec![Theme::MaterialLoss];
    }
    let mut repeated = rows[0].clone();
    repeated.decision.ply += 4;
    rows.push(repeated);
    let report = ranking::aggregate(&rows, 3);
    let first = report.weaknesses.iter().find(|w| w.played_uci == "a2a3").unwrap();
    assert_eq!(first.example.decision.game_id, "z-confirmed");
    assert_eq!(first.occurrences, 2);
    assert_eq!(report.themes.len(), 1);
    assert_eq!(report.themes[0].positions, 2);
    assert_eq!(report.themes[0].games, 3);
    assert_eq!(report.themes[0].occurrences, 3);
    assert_eq!(first.themes, [Theme::MaterialLoss]);
    rows.reverse();
    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        serde_json::to_value(ranking::aggregate(&rows, 3)).unwrap()
    );
}

#[cfg(feature = "uci")]
#[tokio::test]
#[ignore = "requires STOCKFISH_PATH; run explicitly against the pinned release"]
async fn stockfish_scores_mate_from_each_players_perspective() {
    let path = std::env::var("STOCKFISH_PATH").expect("STOCKFISH_PATH");
    let mut engine = chess_coach::uci::Stockfish::spawn(path).await.unwrap();
    for (color, root_move, expected) in [
        (Color::White, Some("g2g4".to_owned()), Score::Mate(-1)),
        (Color::Black, None, Score::Mate(1)),
    ] {
        let decision = decisions(
            &Game {
                id: "mate".into(),
                moves_san: "f3 e5 g4 Qh4#".into(),
                player_color: color,
            },
            15,
        )
        .unwrap()
        .pop()
        .unwrap();
        let result = engine
            .evaluate(&EvaluationRequest {
                position: decision.position,
                profile: EngineProfile {
                    engine_name: "Stockfish 19".into(),
                    nodes: 100_000,
                    timeout_ms: 30_000,
                    ..profile()
                },
                root_move,
            })
            .await
            .unwrap();
        assert_eq!(result.score, expected);
    }
}
