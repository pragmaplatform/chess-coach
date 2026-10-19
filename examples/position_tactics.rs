//! Scan a serialized Position, or the default sample, without producing a mistake report.
use std::time::Instant;

use chess_coach::{
    engine::EngineProfile,
    game::{Color, Game, Position, decisions},
    tactics,
    uci::Stockfish,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let executable = args
        .next()
        .ok_or("usage: position_tactics /path/to/stockfish [position.json]")?;
    let position: Position = match args.next() {
        Some(path) => serde_json::from_slice(&std::fs::read(path)?)?,
        None => {
            decisions(
                &Game {
                    id: "piece-offer".into(),
                    moves_san: "e4 e6 d4 d5 Nc3 Nf6 e5 Nfd7 Bd3 Be7 Nf3 O-O h4 c5 a3".into(),
                    player_color: Color::White,
                },
                u16::MAX,
            )?
            .pop()
            .ok_or("Missing sample position")?
            .position
        }
    };
    let profiles = [100_000, 500_000].map(|nodes| EngineProfile {
        engine_name: "Stockfish 19".into(),
        threads: 1,
        hash_mb: 64,
        nodes,
        timeout_ms: 30_000,
    });
    let mut engine = Stockfish::spawn(executable).await?;
    let started = Instant::now();
    let scan = tactics::scan_position(&position, profiles, &mut engine).await?;
    eprintln!(
        "{} candidates, {} findings, elapsed {:?}",
        scan.candidate_moves.len(),
        scan.opportunities.len(),
        started.elapsed()
    );
    println!("{}", serde_json::to_string_pretty(&scan)?);
    Ok(())
}
