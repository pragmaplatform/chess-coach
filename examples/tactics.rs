use std::time::Instant;

use chess_coach::{
    analysis::Observation,
    engine::{Engine, EngineProfile, EvaluationConfidence, EvaluationRequest, loss},
    game::{Color, Game, decisions},
    tactics,
    uci::Stockfish,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let executable = std::env::args().nth(1).ok_or("Pass the Stockfish executable path")?;
    let mut engine = Stockfish::spawn(executable).await?;
    let game = Game {
        id: "greek-gift".into(),
        moves_san: std::env::args()
            .nth(2)
            .unwrap_or_else(|| "e4 e6 d4 d5 Nc3 Nf6 e5 Nfd7 Bd3 Be7 Nf3 O-O h4 c5 a3".into()),
        player_color: Color::White,
    };
    let decision = decisions(&game, 15)?.pop().ok_or("Missing example position")?;
    let profiles = [100_000, 500_000].map(|nodes| EngineProfile {
        engine_name: "Stockfish 19".into(),
        threads: 1,
        hash_mb: 64,
        nodes,
        timeout_ms: 30_000,
    });
    let request = EvaluationRequest {
        position: decision.position.clone(),
        profile: profiles[1].clone(),
        root_move: None,
    };
    let best = engine.evaluate(&request).await?;
    let played = engine
        .evaluate(&EvaluationRequest {
            root_move: Some(decision.played_uci.clone()),
            ..request
        })
        .await?;
    let observation = Observation {
        loss: loss(best.score, played.score),
        mistake: loss(best.score, played.score) >= 0.05,
        decision,
        best,
        played,
        profile: profiles[1].clone(),
        confidence: EvaluationConfidence::Unverified,
        teaching: None,
        severity: None,
        cloud_evaluations: None,
        tactics: None,
    };
    eprintln!(
        "best={}, candidate={}",
        observation.best.best_move,
        tactics::is_candidate(&observation)?
    );
    let started = Instant::now();
    let scan = tactics::scan(&observation, profiles, &mut engine).await?;
    eprintln!("verification elapsed: {:?}", started.elapsed());
    println!("{}", serde_json::to_string_pretty(&scan)?);
    Ok(())
}
