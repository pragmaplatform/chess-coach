//! Fixed compatibility data, independent of the implementation being tested.
use chess_coach::review::{FsrsConfig, ReviewState, ReviewTransition};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
struct Fixture {
    name: String,
    config: FsrsConfig,
    config_id: String,
    parameters_id: String,
    decay: f32,
    initial_state: ReviewState,
    transitions: Vec<ReviewTransition>,
    recall_vectors: Vec<RecallVector>,
}

#[derive(Deserialize)]
struct RecallVector {
    stability_days: f32,
    last_review_at: i64,
    now: i64,
    elapsed_days: u32,
    recall: f32,
}

fn fixtures() -> Vec<Fixture> {
    serde_json::from_str(include_str!("fixtures/review/fsrs6_v1.json")).unwrap()
}

fn json(value: &impl Serialize) -> serde_json::Value {
    // Use the serialized f32 representation, as on the wire, rather than widening
    // floats via to_value before serializing them.
    serde_json::from_str(&serde_json::to_string(value).unwrap()).unwrap()
}

#[test]
fn stored_json_round_trips_without_the_scheduler() {
    let raw: Vec<serde_json::Value> = serde_json::from_str(include_str!("fixtures/review/fsrs6_v1.json")).unwrap();
    for (fixture, raw) in fixtures().iter().zip(raw) {
        assert_eq!(json(&fixture.config), raw["config"]);
        assert_eq!(json(&fixture.initial_state), raw["initial_state"]);
        assert_eq!(json(&fixture.transitions), raw["transitions"]);
        assert!(!fixture.name.is_empty());
        assert_eq!(fixture.initial_state.parameters_id, fixture.parameters_id);
        assert!(
            fixture
                .transitions
                .iter()
                .all(|event| event.config_id == fixture.config_id)
        );
        assert_eq!(fixture.decay, fixture.config.parameters[20]);
        for vector in &fixture.recall_vectors {
            assert!(vector.stability_days > 0.0);
            assert_eq!(
                (vector.now - vector.last_review_at) / 86_400,
                i64::from(vector.elapsed_days)
            );
            assert!((0.0..=1.0).contains(&vector.recall));
        }
    }
}

#[cfg(feature = "fsrs")]
#[test]
fn persisted_identities_and_scheduling_remain_compatible() {
    use chess_coach::review::FsrsScheduler;
    for fixture in fixtures() {
        if fixture.name == "default" {
            assert_eq!(fixture.config, FsrsConfig::default());
        }
        let scheduler = FsrsScheduler::new(fixture.config).unwrap();
        assert_eq!(scheduler.config_id(), fixture.config_id);
        assert_eq!(scheduler.parameters_id(), fixture.parameters_id);
        assert_eq!(scheduler.decay(), fixture.decay);
        assert_eq!(scheduler.new_state(fixture.initial_state.due_at), fixture.initial_state);
        let mut state = fixture.initial_state;
        for expected in fixture.transitions {
            let mut actual = scheduler
                .review(&state, expected.event.rating, expected.event.at)
                .unwrap();
            let memory = actual.state.memory.unwrap().memory;
            let expected_memory = expected.state.memory.unwrap().memory;
            assert!(
                (memory.stability_days - expected_memory.stability_days).abs() <= 1e-5 * expected_memory.stability_days
            );
            assert!((memory.difficulty - expected_memory.difficulty).abs() <= 1e-5);
            // All discrete contract fields are exact; only floating-point math has tolerance.
            actual.state.memory.as_mut().unwrap().memory = expected_memory;
            assert_eq!(actual, expected);
            // The next review resumes the persisted fixture, not freshly computed state.
            state = expected.state;
        }
    }
}

#[cfg(feature = "fsrs")]
#[test]
fn recall_vectors_match_scheduler_and_database_formula() {
    use chess_coach::review::FsrsScheduler;
    for fixture in fixtures() {
        let scheduler = FsrsScheduler::new(fixture.config).unwrap();
        assert_eq!(
            scheduler
                .retrievability(&fixture.initial_state, fixture.initial_state.due_at)
                .unwrap(),
            None
        );
        for vector in fixture.recall_vectors {
            let mut state = fixture.transitions[0].state.clone();
            state.memory.as_mut().unwrap().memory.stability_days = vector.stability_days;
            state.memory.as_mut().unwrap().last_review_at = vector.last_review_at;
            let actual = scheduler.retrievability(&state, vector.now).unwrap().unwrap();
            assert!((actual - vector.recall).abs() <= 1e-6);
            let days = ((vector.now - vector.last_review_at) as f64 / 86_400.0).floor();
            let decay = f64::from(scheduler.decay());
            let sql = (1.0 + days / f64::from(vector.stability_days) * (0.9_f64.powf(-1.0 / decay) - 1.0)).powf(-decay);
            assert!((sql - f64::from(vector.recall)).abs() <= 1e-6);
        }
    }
}
