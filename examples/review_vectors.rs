//! Explicitly regenerate the compatibility fixture when reviewing a contract change.
//! Never run this automatically before compatibility tests.
use chess_coach::review::{FsrsConfig, FsrsScheduler, RecallRating, ReviewState, ReviewTransition};
use serde::Serialize;

#[derive(Serialize)]
struct Fixture {
    name: &'static str,
    config: FsrsConfig,
    config_id: String,
    parameters_id: String,
    decay: f32,
    initial_state: ReviewState,
    transitions: Vec<ReviewTransition>,
    recall_vectors: Vec<RecallVector>,
}

#[derive(Serialize)]
struct RecallVector {
    stability_days: f32,
    last_review_at: i64,
    now: i64,
    elapsed_days: u32,
    recall: f32,
}

fn fixture(name: &'static str, config: FsrsConfig) -> Result<Fixture, chess_coach::Error> {
    let scheduler = FsrsScheduler::new(config.clone())?;
    let start = 1_800_000_000;
    let initial_state = scheduler.new_state(start);
    let mut state = initial_state.clone();
    let mut transitions = Vec::new();
    for (offset, rating) in [
        (0, RecallRating::Good),
        (0, RecallRating::Again),
        (2 * 86_400, RecallRating::Hard),
        (47 * 86_400, RecallRating::Easy),
    ] {
        let transition = scheduler.review(&state, rating, start + offset)?;
        state = transition.state.clone();
        transitions.push(transition);
    }
    let mut recall_vectors = Vec::new();
    for (stability_days, seconds) in [
        (10.0, 0),
        (10.0, 1),
        (10.0, 86_399),
        (10.0, 86_400),
        (10.0, 86_401),
        (10.0, 172_799),
        (10.0, 172_800),
        (10.0, 10 * 86_400),
        (10.0, 60 * 86_400),
        (0.001, 86_400),
        (1.0, 30 * 86_400),
        (36_500.0, 36_500 * 86_400),
    ] {
        let mut memory = transitions[0].state.clone();
        memory.memory.as_mut().unwrap().memory.stability_days = stability_days;
        let now = start + seconds;
        recall_vectors.push(RecallVector {
            stability_days,
            last_review_at: start,
            now,
            elapsed_days: (seconds / 86_400) as u32,
            recall: scheduler.retrievability(&memory, now)?.unwrap(),
        });
    }
    Ok(Fixture {
        name,
        config,
        config_id: scheduler.config_id().to_owned(),
        parameters_id: scheduler.parameters_id().to_owned(),
        decay: scheduler.decay(),
        initial_state,
        transitions,
        recall_vectors,
    })
}

fn main() -> Result<(), chess_coach::Error> {
    let mut custom = FsrsConfig {
        desired_retention: 0.95,
        maximum_interval_days: 30,
        ..Default::default()
    };
    custom.parameters[20] = 0.5;
    println!(
        "{}",
        serde_json::to_string_pretty(&[
            fixture("default", FsrsConfig::default())?,
            fixture("custom_decay_retention_and_limit", custom)?,
        ])?
    );
    Ok(())
}
