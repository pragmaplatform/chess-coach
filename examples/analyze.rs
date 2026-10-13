//! Analyze a canonical SAN move list, e.g. the `moves` field in a Lichess export.
use chess_coach::{
    analysis::{AnalysisPolicy, analyze_game},
    engine::EngineProfile,
    game::Game,
    ranking,
    uci::Stockfish,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let executable = args.next().ok_or("usage: analyze /path/to/stockfish game.json")?;
    let game: Game = serde_json::from_slice(&std::fs::read(args.next().ok_or("missing game.json")?)?)?;
    let policy = AnalysisPolicy {
        version: "openingcoach-logistic-v4".into(),
        full_moves: 15,
        minimum_loss: 0.05,
        opening_threshold: Some(Default::default()),
        acceptable_loss: 0.02,
        engine: EngineProfile {
            engine_name: "Stockfish 19".into(),
            threads: 3,
            hash_mb: 64,
            nodes: 100_000,
            timeout_ms: 30_000,
        },
        confirmation_nodes: 1_000_000,
        verification_nodes: Some(5_000_000),
        cloud_min_depth: None,
    };
    let mut engine = Stockfish::spawn(executable).await?;
    let started = std::time::Instant::now();
    let observations = analyze_game(&game, &policy, &mut engine).await?;
    eprintln!(
        "Analyzed {} opening decisions in {:.2}s",
        observations.len(),
        started.elapsed().as_secs_f64()
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&ranking::aggregate(&observations, 1))?
    );
    Ok(())
}
