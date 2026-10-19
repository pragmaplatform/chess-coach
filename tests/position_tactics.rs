use chess_coach::{
    Error,
    engine::{Engine, EngineProfile, Evaluation, EvaluationRequest, EvaluationSource, Score},
    game::{self, Color, Game, Position},
    tactics::{self, Motif},
};

#[tokio::test]
async fn alternatives_can_give_up_a_large_advantage_and_root_searches_are_shared() {
    let mut engine = FixtureEngine::new(false, false);
    let position = engine.position.clone();
    let candidates = tactics::candidate_moves(&position).unwrap();
    assert!(candidates.len() > 1);
    let result = tactics::scan_position(&position, profiles(), &mut engine)
        .await
        .unwrap();
    assert_eq!(result.candidate_moves, candidates);
    assert_eq!(result.position, position);
    assert_eq!(result.best.unwrap().best_move, "h2h4");
    assert_eq!(result.opportunities.len(), 1);
    let offer = &result.opportunities[0];
    assert_eq!(offer.move_uci, "d3h7");
    assert_eq!(offer.sacrifice.score, Score::Centipawns(0));
    assert_eq!(offer.advantage_given_up_cp, Some(400));
    assert!(offer.loss > 0.30);
    assert_eq!(offer.motifs, [Motif::Sacrifice, Motif::Trap]);
    assert_eq!(offer.reply_position.history_uci.last().unwrap(), "d3h7");
    assert_eq!(offer.best_defence.score, Score::Centipawns(0));
    assert_eq!(offer.capture.score, Score::Centipawns(-500));
    assert_eq!(
        engine
            .requests
            .iter()
            .filter(|r| r.position == position && r.root_move.is_none())
            .count(),
        2
    );
    assert!(engine.requests.len() <= 2 + 6 * candidates.len());
    let json = serde_json::to_value(&result.opportunities).unwrap();
    assert!(
        json[0].get("role").is_none(),
        "A position scan must not claim a missed move"
    );
}

#[tokio::test]
async fn equal_offers_survive_changes_to_the_unrestricted_best_score() {
    for (best_scores, expected_loss_cp) in [
        ([Score::Centipawns(400), Score::Centipawns(460)], Some(460)),
        ([Score::Centipawns(460), Score::Centipawns(400)], Some(400)),
        ([Score::Centipawns(400), Score::Mate(3)], None),
    ] {
        let mut engine = FixtureEngine::new(false, false);
        engine.best_scores = best_scores;
        let result = tactics::scan_position(&engine.position.clone(), profiles(), &mut engine)
            .await
            .unwrap();
        assert_eq!(result.best.unwrap().score, best_scores[1]);
        assert_eq!(result.opportunities.len(), 1, "best scores: {best_scores:?}");
        let offer = &result.opportunities[0];
        assert_eq!(offer.move_uci, engine.offer);
        assert_eq!(offer.sacrifice.score, Score::Centipawns(0));
        assert_eq!(offer.best_defence.score, Score::Centipawns(0));
        assert_eq!(offer.advantage_given_up_cp, expected_loss_cp);
    }
}

#[tokio::test]
async fn rejected_initial_offers_skip_confirmation_and_reply_searches() {
    for inconclusive in [false, true] {
        let mut engine = FixtureEngine::new(false, false);
        if inconclusive {
            engine.best_scores[0] = Score::Centipawns(-20);
        } else {
            engine.offer_scores[0] = Score::Centipawns(-1);
        }
        let result = tactics::scan_position(&engine.position.clone(), profiles(), &mut engine)
            .await
            .unwrap();
        assert!(result.opportunities.is_empty());
        assert_eq!(engine.requests.len(), 2 + result.candidate_moves.len());
        assert!(engine.requests.iter().all(|r| r.position == engine.position));
        assert!(
            engine
                .requests
                .iter()
                .filter(|r| r.profile.nodes == profiles()[1].nodes)
                .all(|r| r.root_move.is_none())
        );
    }
}

#[tokio::test]
async fn rejected_initial_replies_skip_reply_confirmation() {
    for case in 0..3 {
        let mut engine = FixtureEngine::new(false, false);
        match case {
            0 => engine.defence_scores[0] = Score::Centipawns(1),
            1 => engine.defence_scores[0] = Score::Centipawns(-1000),
            _ => engine.reply_uses_cloud = true,
        }
        let result = tactics::scan_position(&engine.position.clone(), profiles(), &mut engine)
            .await
            .unwrap();
        assert!(result.opportunities.is_empty(), "case {case}");
        let replies: Vec<_> = engine
            .requests
            .iter()
            .filter(|r| r.position != engine.position)
            .collect();
        assert!(!replies.is_empty());
        assert!(replies.iter().all(|r| r.profile.nodes == profiles()[0].nodes));
    }
}

#[tokio::test]
async fn scans_black_and_positions_after_move_fifteen_with_full_history() {
    for (black, late) in [(true, false), (false, true)] {
        let mut engine = FixtureEngine::new(black, late);
        let position = engine.position.clone();
        if late {
            assert!(position.history_uci.len() > 30);
        }
        let result = tactics::scan_position(&position, profiles(), &mut engine)
            .await
            .unwrap();
        assert_eq!(result.opportunities.len(), 1);
        let offer = &result.opportunities[0];
        assert_eq!(offer.move_uci, engine.offer);
        assert_eq!(offer.advantage_given_up_cp, Some(400));
        assert!(
            engine
                .requests
                .iter()
                .all(|r| r.position.history_uci.starts_with(&position.history_uci))
        );
        assert_eq!(offer.reply_position.history_uci.len(), position.history_uci.len() + 1);
        game::line_san(&offer.reply_position, &offer.capture.line).unwrap();
    }
}

#[tokio::test]
async fn best_move_offers_are_reused_and_best_defence_captures_are_not_traps() {
    let mut engine = FixtureEngine::new(false, false);
    engine.best_is_offer = true;
    engine.capture_is_best = true;
    let position = engine.position.clone();
    let result = tactics::scan_position(&position, profiles(), &mut engine)
        .await
        .unwrap();
    assert_eq!(result.opportunities.len(), 1);
    assert_eq!(result.opportunities[0].advantage_given_up_cp, Some(0));
    assert_eq!(result.opportunities[0].motifs, [Motif::Sacrifice]);
    assert!(
        !engine
            .requests
            .iter()
            .any(|r| r.position == position && r.root_move.as_deref() == Some(engine.offer))
    );
}

#[tokio::test]
async fn unsound_unstable_mismatched_and_immediately_recovered_offers_are_excluded() {
    for case in 0..10 {
        let mut engine = FixtureEngine::new(false, false);
        match case {
            0 => engine.offer_scores = [Score::Centipawns(-1); 2],
            1 => engine.offer_scores[1] = Score::Centipawns(100),
            2 => engine.defence_scores = [Score::Centipawns(1); 2],
            3 => engine.defence_scores = [Score::Centipawns(-100); 2],
            4 => engine.offer_scores[0] = Score::Centipawns(-1),
            5 => engine.defence_scores[0] = Score::Centipawns(1),
            6 => engine.recover_material = true,
            7 => engine.source_changes = true,
            8 => engine.best_scores[0] = Score::Centipawns(-20),
            _ => engine.reply_uses_cloud = true,
        }
        let result = tactics::scan_position(&engine.position.clone(), profiles(), &mut engine)
            .await
            .unwrap();
        assert!(result.opportunities.is_empty(), "case {case}");
    }
}

#[tokio::test]
async fn mate_scores_are_not_fabricated_as_centipawn_loss() {
    let mut engine = FixtureEngine::new(false, false);
    engine.best_scores = [Score::Mate(3); 2];
    let result = tactics::scan_position(&engine.position.clone(), profiles(), &mut engine)
        .await
        .unwrap();
    assert_eq!(result.opportunities.len(), 1);
    assert_eq!(result.opportunities[0].advantage_given_up_cp, None);
    assert_eq!(result.opportunities[0].loss, 0.5);

    // A mate claim at the original root must agree with the opponent-root evidence.
    engine.offer_scores = [Score::Mate(5); 2];
    engine.defence_scores = [Score::Centipawns(-5000); 2];
    let result = tactics::scan_position(&engine.position.clone(), profiles(), &mut engine)
        .await
        .unwrap();
    assert!(result.opportunities.is_empty());
}

#[tokio::test]
async fn empty_and_terminal_positions_need_no_engine_work() {
    for (san, color, finish) in [("e4", Color::White, false), ("f3 e5 g4 Qh4#", Color::Black, true)] {
        let decision = game::decisions(
            &Game {
                id: "no-offers".into(),
                moves_san: san.into(),
                player_color: color,
            },
            15,
        )
        .unwrap()
        .pop()
        .unwrap();
        let position = if finish {
            game::position_after(&decision.position, &[decision.played_uci]).unwrap()
        } else {
            decision.position
        };
        let mut engine = FixtureEngine::new(false, false);
        let result = tactics::scan_position(&position, profiles(), &mut engine)
            .await
            .unwrap();
        assert!(result.candidate_moves.is_empty());
        assert!(result.opportunities.is_empty());
        assert!(result.best.is_none());
        assert!(engine.requests.is_empty());
    }
}

#[tokio::test]
async fn invalid_profiles_positions_and_engine_results_are_errors() {
    let mut engine = FixtureEngine::new(false, false);
    for case in 0..4 {
        let mut budgets = profiles();
        match case {
            0 => budgets[1].nodes = budgets[0].nodes,
            1 => budgets[1].engine_name = "other".into(),
            2 => budgets[0].timeout_ms = 0,
            _ => budgets[1].threads += 1,
        }
        assert!(
            tactics::scan_position(&engine.position.clone(), budgets, &mut engine)
                .await
                .is_err()
        );
    }
    let mut invalid = engine.position.clone();
    invalid.history_uci.clear();
    assert!(tactics::scan_position(&invalid, profiles(), &mut engine).await.is_err());
    assert!(engine.requests.is_empty());
    for failure in [
        Failure::Search,
        Failure::MissingLine,
        Failure::IllegalLine,
        Failure::WrongMove,
    ] {
        let mut engine = FixtureEngine::new(false, false);
        engine.failure = Some(failure);
        assert!(
            tactics::scan_position(&engine.position.clone(), profiles(), &mut engine)
                .await
                .is_err()
        );
    }
}

#[test]
fn candidate_shortlist_includes_attacked_pieces_and_excludes_routine_exchanges() {
    let position = game::decisions(
        &Game {
            id: "attacked".into(),
            moves_san: "e4 e6 d4 d5 Nc3 Nf6 e5 Nfd7 Bd3 Be7 Nf3 O-O h4 Nc6 a3 Nb4 b3".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .pop()
    .unwrap()
    .position;
    assert!(
        tactics::candidate_moves(&position)
            .unwrap()
            .contains(&"d3h7".to_owned())
    );
    let exchange = game::decisions(
        &Game {
            id: "exchange".into(),
            moves_san: "e4 e5 Nf3 Nc6 Bb5 Nf6 d3".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .pop()
    .unwrap()
    .position;
    assert!(
        !tactics::candidate_moves(&exchange)
            .unwrap()
            .contains(&"b5c6".to_owned())
    );
    let evasion = game::decisions(
        &Game {
            id: "check-evasion".into(),
            moves_san: "e4 e5 d4 Nc6 Nf3 Bb4+ Nc3".into(),
            player_color: Color::White,
        },
        15,
    )
    .unwrap()
    .pop()
    .unwrap()
    .position;
    assert!(tactics::candidate_moves(&evasion).unwrap().contains(&"b1c3".to_owned()));
}

#[cfg(feature = "openapi")]
#[test]
fn position_scan_schema_is_available_without_application_types() {
    use utoipa::OpenApi;
    #[derive(OpenApi)]
    #[openapi(components(schemas(tactics::PositionScan, tactics::PositionOpportunity)))]
    struct Api;
    let schema = serde_json::to_value(Api::openapi()).unwrap();
    assert!(
        schema["components"]["schemas"]["ChessPositionTacticalOpportunity"]["properties"]["advantage_given_up_cp"]
            .is_object()
    );
}

fn profiles() -> [EngineProfile; 2] {
    [100, 500].map(|nodes| EngineProfile {
        engine_name: "fixture".into(),
        threads: 1,
        hash_mb: 16,
        nodes,
        timeout_ms: 1000,
    })
}

enum Failure {
    Search,
    MissingLine,
    IllegalLine,
    WrongMove,
}

struct FixtureEngine {
    position: Position,
    offer: &'static str,
    quiet: &'static str,
    defence: &'static str,
    capture: &'static str,
    continuation: &'static str,
    best_scores: [Score; 2],
    offer_scores: [Score; 2],
    defence_scores: [Score; 2],
    best_is_offer: bool,
    capture_is_best: bool,
    recover_material: bool,
    source_changes: bool,
    reply_uses_cloud: bool,
    failure: Option<Failure>,
    requests: Vec<EvaluationRequest>,
}

impl FixtureEngine {
    fn new(black: bool, late: bool) -> Self {
        let prefix = if late {
            "Nf3 Nf6 Ng5 Ng4 Nf3 Nf6 Ng1 Ng8 Nc3 Nc6 Nb5 Nb4 Nc3 Nc6 Nb1 Nb8 Nf3 Nc6 Ng1 Nb8 "
        } else {
            ""
        };
        let san = if black {
            "a3 e5 e3 d5 d4 Nc6 Nf3 e4 Nfd2 Bd6 Be2 Nf6 O-O h5"
        } else {
            "e4 e6 d4 d5 Nc3 Nf6 e5 Nfd7 Bd3 Be7 Nf3 O-O h4"
        };
        let position = game::decisions(
            &Game {
                id: "offer".into(),
                moves_san: format!("{prefix}{san}"),
                player_color: if black { Color::Black } else { Color::White },
            },
            u16::MAX,
        )
        .unwrap()
        .pop()
        .unwrap()
        .position;
        Self {
            position,
            offer: if black { "d6h2" } else { "d3h7" },
            quiet: if black { "h7h5" } else { "h2h4" },
            defence: if black { "g1h1" } else { "g8h8" },
            capture: if black { "g1h2" } else { "g8h7" },
            continuation: if black { "f6g4" } else { "f3g5" },
            best_scores: [Score::Centipawns(400); 2],
            offer_scores: [Score::Centipawns(0); 2],
            defence_scores: [Score::Centipawns(0); 2],
            best_is_offer: false,
            capture_is_best: false,
            recover_material: false,
            source_changes: false,
            reply_uses_cloud: false,
            failure: None,
            requests: Vec::new(),
        }
    }
}

#[async_trait::async_trait]
impl Engine for FixtureEngine {
    async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
        self.requests.push(request.clone());
        if matches!(self.failure, Some(Failure::Search)) {
            return Err(Error::Engine("fixture unavailable".into()));
        }
        let stage = usize::from(request.profile.nodes == 500);
        let (mv, score, line) = if request.position == self.position {
            let mv = request
                .root_move
                .as_deref()
                .unwrap_or(if self.best_is_offer { self.offer } else { self.quiet });
            let score = if mv == self.offer {
                self.offer_scores[stage]
            } else if request.root_move.is_none() {
                self.best_scores[stage]
            } else {
                Score::Centipawns(-100)
            };
            (mv, score, vec![mv.to_owned()])
        } else {
            assert_eq!(request.position.history_uci.last().unwrap(), self.offer);
            let mv = request.root_move.as_deref().unwrap_or(if self.capture_is_best {
                self.capture
            } else {
                self.defence
            });
            if mv == self.capture {
                (
                    mv,
                    if self.capture_is_best {
                        self.defence_scores[stage]
                    } else {
                        Score::Centipawns(-500)
                    },
                    vec![
                        mv.to_owned(),
                        if self.recover_material {
                            "c3d5"
                        } else {
                            self.continuation
                        }
                        .to_owned(),
                    ],
                )
            } else {
                (mv, self.defence_scores[stage], vec![mv.to_owned()])
            }
        };
        let mut evaluation = Evaluation {
            source: if (self.source_changes && stage == 1)
                || (self.reply_uses_cloud && request.position != self.position)
            {
                EvaluationSource::LichessCloud
            } else {
                EvaluationSource::Stockfish
            },
            score,
            best_move: mv.to_owned(),
            line,
            depth: 20,
            nodes: request.profile.nodes,
        };
        match self.failure {
            Some(Failure::MissingLine) => evaluation.line.clear(),
            Some(Failure::IllegalLine) => evaluation.line.push("a1a8".into()),
            Some(Failure::WrongMove) if request.root_move.is_some() => {
                evaluation.best_move = self.quiet.into();
                evaluation.line = vec![self.quiet.into()];
            }
            _ => {}
        }
        Ok(evaluation)
    }
}
