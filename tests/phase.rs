use chess_coach::{
    Error,
    analysis::{AnalysisPolicy, Observation, analyze_game},
    engine::{Engine, EngineProfile, Evaluation, EvaluationRequest, Score},
    game::{self, Color, Game, Position},
    phase::{self, Phase},
    ranking,
};

const OPERA: &str = "e4 e5 Nf3 d6 d4 Bg4 dxe5 Bxf3 Qxf3 dxe5 Bc4 Nf6 Qb3 Qe7 Nc3 c6 Bg5 b5 Nxb5 cxb5 Bxb5+ Nbd7 O-O-O Rd8 Rxd7 Rxd7 Rd1 Qe6 Bxd7+ Nxd7 Qb8+ Nxb8 Rd8#";

#[test]
fn classifies_both_sides_of_a_game_and_the_position_before_a_move() {
    for color in [Color::White, Color::Black] {
        let decisions = game::decisions(&game(color), 100).unwrap();
        let phases: Vec<_> = decisions
            .iter()
            .map(|d| phase::classify(&d.position).unwrap())
            .collect();
        assert_eq!(phases[0], Phase::Opening);
        assert!(phases.contains(&Phase::Middlegame));
        assert!(phases.windows(2).all(|pair| pair[0] <= pair[1]));
        for decision in &decisions {
            let before = phase::classify(&decision.position).unwrap();
            let after = game::position_after(&decision.position, std::slice::from_ref(&decision.played_uci)).unwrap();
            if decision.played_san == "Nxb8" {
                assert_eq!(before, Phase::Middlegame);
                assert_eq!(phase::classify(&after).unwrap(), Phase::Endgame);
            }
            if decision.played_san == "Rd8#" {
                assert_eq!(before, Phase::Endgame);
                assert_eq!(phase::classify(&after).unwrap(), Phase::Endgame);
            }
        }
    }
}

#[test]
fn returning_to_the_same_board_keeps_the_phase_reached_in_history() {
    let game = Game {
        id: "undevelopment".into(),
        moves_san: "e4 Nf6 d4 Ng8 Nf3 Nf6 Nc3 Ng8 Bd3 Nf6 Be3 Ng8 Qd2 Nf6 Qd1 Ng8 Bc1 Nf6 Bf1 Ng8 Nb1 Nf6 Ng1 Ng8 e5"
            .into(),
        player_color: Color::White,
    };
    let decisions = game::decisions(&game, 100).unwrap();
    let opening = &decisions[2].position;
    let returned = &decisions.last().unwrap().position;
    assert_eq!(opening.key, returned.key);
    assert_eq!(phase::classify(opening).unwrap(), Phase::Opening);
    assert_eq!(phase::classify(returned).unwrap(), Phase::Middlegame);
}

#[test]
fn validates_complete_history_even_after_reaching_endgame() {
    let mut position = game::decisions(&game(Color::White), 100)
        .unwrap()
        .pop()
        .unwrap()
        .position;
    assert_eq!(phase::classify(&position).unwrap(), Phase::Endgame);
    position.history_uci.push("a1a8".into());
    assert!(phase::classify(&position).is_err());
    let original = game::decisions(&game(Color::White), 100)
        .unwrap()
        .pop()
        .unwrap()
        .position;
    for invalid in [
        Position {
            key: "invalid".into(),
            ..original.clone()
        },
        Position {
            fen: "invalid".into(),
            ..original.clone()
        },
        Position {
            history_uci: Vec::new(),
            ..original
        },
    ] {
        assert!(phase::classify(&invalid).is_err());
    }
}

#[tokio::test]
async fn analysis_tags_observations_without_more_searches_or_changing_the_limit() {
    let policy = AnalysisPolicy {
        version: "phase-test".into(),
        full_moves: 15,
        minimum_loss: 0.05,
        opening_threshold: None,
        acceptable_loss: 0.02,
        engine: EngineProfile {
            engine_name: "fixture".into(),
            threads: 1,
            hash_mb: 16,
            nodes: 100,
            timeout_ms: 1000,
        },
        confirmation_nodes: 100,
        verification_nodes: None,
        cloud_min_depth: None,
    };
    let game = game(Color::White);
    let mut engine = FixtureEngine::default();
    let mut observations = analyze_game(&game, &policy, &mut engine).await.unwrap();
    assert_eq!(observations.len(), 15);
    assert!(observations.iter().all(|o| o.decision.ply <= 30));
    assert_eq!(engine.requests, 2 * observations.len());
    for observation in &observations {
        assert_eq!(
            observation.phase,
            Some(phase::classify(&observation.decision.position).unwrap())
        );
    }
    assert_eq!(observations[0].phase, Some(Phase::Opening));
    assert_eq!(observations.last().unwrap().phase, Some(Phase::Middlegame));

    // Report examples preserve phase without using it as a new eligibility condition.
    observations[0].mistake = true;
    let report = ranking::aggregate(&observations, 1);
    assert_eq!(report.weaknesses[0].example.phase, Some(Phase::Opening));
    let mut legacy = serde_json::to_value(&observations[0]).unwrap();
    legacy.as_object_mut().unwrap().remove("phase");
    let old: Observation = serde_json::from_value(legacy).unwrap();
    assert_eq!(old.phase, None);
    let saved = serde_json::to_value(&observations[0]).unwrap();
    assert_eq!(saved["phase"], "opening");
    assert_eq!(
        serde_json::from_value::<Observation>(saved).unwrap().phase,
        Some(Phase::Opening)
    );
}

#[cfg(feature = "openapi")]
#[test]
fn phase_schema_and_serialization_use_the_same_names() {
    use utoipa::OpenApi;
    #[derive(OpenApi)]
    #[openapi(components(schemas(Observation, Phase)))]
    struct Api;
    let schema = serde_json::to_value(Api::openapi()).unwrap();
    assert_eq!(
        schema["components"]["schemas"]["ChessPhase"]["enum"],
        serde_json::json!(["opening", "middlegame", "endgame"])
    );
    for phase in [Phase::Opening, Phase::Middlegame, Phase::Endgame] {
        let json = serde_json::to_value(phase).unwrap();
        assert_eq!(serde_json::from_value::<Phase>(json).unwrap(), phase);
    }
}

fn game(color: Color) -> Game {
    Game {
        id: "opera".into(),
        moves_san: OPERA.into(),
        player_color: color,
    }
}

#[derive(Default)]
struct FixtureEngine {
    requests: usize,
}

#[async_trait::async_trait]
impl Engine for FixtureEngine {
    async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
        self.requests += 1;
        let board = game::board(&request.position)?;
        let mut legal = Vec::new();
        board.generate_moves(|moves| {
            legal.extend(moves);
            false
        });
        let mv = request
            .root_move
            .clone()
            .unwrap_or_else(|| cozy_chess::util::display_uci_move(&board, legal[0]).to_string());
        Ok(Evaluation {
            source: Default::default(),
            score: Score::Centipawns(0),
            best_move: mv.clone(),
            line: vec![mv],
            depth: 20,
            nodes: request.profile.nodes,
        })
    }
}
