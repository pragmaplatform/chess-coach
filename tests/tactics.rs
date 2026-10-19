use chess_coach::{
    Error,
    analysis::Observation,
    engine::{Engine, EngineProfile, Evaluation, EvaluationConfidence, EvaluationRequest, EvaluationSource, Score},
    game::{Color, Game, decisions},
    tactics::{self, Motif},
};

fn observation() -> Observation {
    let decision = decisions(
        &Game {
            id: "greek-gift".into(),
            moves_san: "e4 e6 d4 d5 Nc3 Nf6 e5 Nfd7 Bd3 Be7 Nf3 O-O h4".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .pop()
    .unwrap();
    Observation {
        decision,
        best: evaluation("d3h7", 100, &["d3h7", "g8h8"]),
        played: evaluation("h2h4", -100, &["h2h4"]),
        profile: profiles()[0].clone(),
        loss: 0.2,
        confidence: EvaluationConfidence::Confirmed,
        teaching: None,
        severity: None,
        mistake: true,
        cloud_evaluations: None,
        tactics: None,
        phase: None,
    }
}

fn profiles() -> [EngineProfile; 2] {
    [100, 500].map(|nodes| EngineProfile {
        engine_name: "test".into(),
        threads: 1,
        hash_mb: 16,
        nodes,
        timeout_ms: 1000,
    })
}

fn evaluation(mv: &str, cp: i32, line: &[&str]) -> Evaluation {
    Evaluation {
        source: EvaluationSource::Stockfish,
        score: Score::Centipawns(cp),
        best_move: mv.into(),
        line: line.iter().map(|mv| (*mv).into()).collect(),
        depth: 20,
        nodes: 100,
    }
}

#[derive(Default)]
struct MockEngine {
    requests: Vec<EvaluationRequest>,
    unsound: bool,
    unstable: bool,
}

#[async_trait::async_trait]
impl Engine for MockEngine {
    async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
        self.requests.push(request.clone());
        let original = request.position.history_uci.len() == 12;
        let cp = if self.unstable && request.profile.nodes == 500 {
            400
        } else {
            100
        };
        Ok(if original {
            if self.unsound && request.root_move.is_some() {
                evaluation("d3h7", -100, &["d3h7", "g8h8"])
            } else if self.unsound {
                evaluation("h2h4", cp, &["h2h4"])
            } else {
                evaluation("d3h7", cp, &["d3h7", "g8h8"])
            }
        } else if request.root_move.as_deref() == Some("g8h7") {
            evaluation("g8h7", -500, &["g8h7", "f3g5", "h7g8"])
        } else {
            evaluation("g8h8", -100, &["g8h8", "h7d3"])
        })
    }
}

#[tokio::test]
async fn sound_sacrifice_and_trap_have_explicit_roots_and_bounded_searches() {
    let observation = observation();
    assert!(tactics::is_candidate(&observation).unwrap());
    let mut engine = MockEngine::default();
    let scan = tactics::scan(&observation, profiles(), &mut engine).await.unwrap();
    assert_eq!(scan.opportunities.len(), 1);
    let opportunity = &scan.opportunities[0];
    assert_eq!(opportunity.motifs, [Motif::Sacrifice, Motif::Trap]);
    assert_eq!(opportunity.move_uci, "d3h7");
    assert_eq!(opportunity.reply_position.history_uci.last().unwrap(), "d3h7");
    assert_eq!(opportunity.best_defence.score, Score::Centipawns(-100));
    assert_eq!(opportunity.capture.score, Score::Centipawns(-500));
    assert_eq!(engine.requests.len(), 6);
    assert!(
        engine
            .requests
            .iter()
            .all(|request| [100, 500].contains(&request.profile.nodes))
    );
}

#[tokio::test]
async fn changed_best_and_unstable_scores_do_not_get_annotated() {
    for mut engine in [
        MockEngine {
            unsound: true,
            ..Default::default()
        },
        MockEngine {
            unstable: true,
            ..Default::default()
        },
    ] {
        let result = tactics::scan(&observation(), profiles(), &mut engine).await;
        assert!(result.unwrap().opportunities.is_empty());
        assert!(engine.requests.len() <= 4);
    }
}

#[tokio::test]
async fn non_candidates_do_not_call_engine_and_old_observations_stay_unchecked() {
    let original = observation();
    let mut old = serde_json::to_value(&original).unwrap();
    old.as_object_mut().unwrap().remove("tactics");
    assert!(serde_json::from_value::<Observation>(old).unwrap().tactics.is_none());
    for variant in 0..3 {
        let mut observation = original.clone();
        match variant {
            0 => observation.mistake = false,
            1 => observation.confidence = EvaluationConfidence::Inconclusive,
            _ => observation.decision.played_uci = observation.best.best_move.clone(),
        }
        let mut engine = MockEngine::default();
        let scan = tactics::scan(&observation, profiles(), &mut engine).await.unwrap();
        assert!(scan.opportunities.is_empty());
        assert!(engine.requests.is_empty());
    }
    let mut pawn = original;
    pawn.best = evaluation("h2h4", 100, &["h2h4"]);
    assert!(!tactics::is_candidate(&pawn).unwrap());
}

#[test]
fn routine_exchange_is_not_a_sacrifice_candidate() {
    let mut observation = observation();
    observation.decision = decisions(
        &Game {
            id: "exchange".into(),
            moves_san: "e4 e5 Nf3 Nc6 Bb5 Nf6 d3".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .pop()
    .unwrap();
    observation.best = evaluation("b5c6", 100, &["b5c6", "d7c6"]);
    assert!(!tactics::is_candidate(&observation).unwrap());
}

#[test]
fn an_already_capturable_piece_is_not_a_new_offer() {
    let mut observation = observation();
    observation.decision = decisions(
        &Game {
            id: "already-attacked".into(),
            moves_san: "e4 e6 d4 d5 Nc3 Nf6 e5 Nfd7 Bd3 Be7 Nf3 O-O h4 Nc6 a3 Nb4 b3".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .pop()
    .unwrap();
    assert!(!tactics::is_candidate(&observation).unwrap());
}
