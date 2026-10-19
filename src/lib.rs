#![doc = include_str!("../README.md")]

/// Analysis policies, candidate confirmation, and per-game observations.
pub mod analysis;
/// Engine integration contracts, scores, and comparison confidence.
pub mod engine;
/// Standard-chess replay, validated positions, and opening decisions.
pub mod game;
/// Opening-family classification from SAN move lists.
pub mod openings;
/// Single-move grading and follow-up positions.
pub mod practice;
/// Weakness aggregation and practice prioritization.
pub mod ranking;
/// Serializable review history and optional FSRS memory scheduling.
pub mod review;
/// Opt-in sacrifice verification for observations or standalone game positions.
pub mod tactics;
/// Explanations supported by the evaluated continuations.
pub mod teaching;
/// Optional Tokio-based adapter for an external Stockfish process.
#[cfg(feature = "uci")]
pub mod uci;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid game: {0}")]
    InvalidGame(String),
    #[error("Invalid configuration: {0}")]
    Configuration(String),
    #[error(transparent)]
    Review(#[from] review::ReviewError),
    #[error("Engine error: {0}")]
    Engine(String),
    #[error("Engine timed out")]
    Timeout,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    External(Box<dyn std::error::Error + Send + Sync>),
}

/// SHA-256 of the value's JSON encoding. Cache callers must include every search input.
pub fn fingerprint(value: &impl serde::Serialize) -> Result<String, Error> {
    use sha2::{Digest, Sha256};
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
}
