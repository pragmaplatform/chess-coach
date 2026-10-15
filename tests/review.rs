use chess_coach::{
    practice::Grade,
    review::{FsrsConfig, LegacyReviewState, RecallRating, ReviewState},
};
use serde_json::json;

#[test]
fn move_quality_is_not_recall_difficulty() {
    for grade in [Grade::Correct, Grade::Good] {
        assert_eq!(RecallRating::from_grade(grade), Some(RecallRating::Good));
    }
    assert_eq!(RecallRating::from_grade(Grade::Incorrect), Some(RecallRating::Again));
    assert_eq!(RecallRating::from_grade(Grade::Inconclusive), None);
    assert_eq!(serde_json::to_value(RecallRating::Hard).unwrap(), "hard");
}

fn stored_state() -> serde_json::Value {
    json!({
        "version": "fsrs6_v1",
        "memory": {"memory": {"stability_days": 14.0, "difficulty": 5.0}, "last_review_at": 100},
        "due_at": 1209700,
        "reviews": 12,
        "lapses": 2,
        "parameters_id": "persisted-parameter-fingerprint",
        "origin": "estimated",
        "legacy": {"step": 4, "reviews": 12, "lapses": 2, "due_at": 1209700}
    })
}

#[test]
fn persisted_types_work_without_the_scheduler_feature() {
    let state: ReviewState = serde_json::from_value(stored_state()).unwrap();
    assert_eq!(serde_json::to_value(&state).unwrap(), stored_state());
    let legacy_json = json!({"step": 6, "reviews": 12, "lapses": 2, "due_at": 1234});
    let legacy: LegacyReviewState = serde_json::from_value(legacy_json.clone()).unwrap();
    assert_eq!(serde_json::to_value(legacy).unwrap(), legacy_json);
    // Old JSON must take the explicit migration path, never deserialize as new.
    assert!(serde_json::from_value::<ReviewState>(legacy_json).is_err());
    let config_json = json!({
        "version": "fsrs6_v1", "parameters": vec![0.1_f32; 21],
        "desired_retention": 0.9_f32, "maximum_interval_days": 36500
    });
    let config: FsrsConfig = serde_json::from_value(config_json.clone()).unwrap();
    assert_eq!(serde_json::to_value(config).unwrap(), config_json);
}

#[test]
fn invalid_persisted_state_is_rejected_instead_of_reset() {
    for (field, value) in [
        ("version", json!("fsrs7_v1")),
        (
            "memory",
            json!({"memory": {"stability_days": 0, "difficulty": 5}, "last_review_at": 100}),
        ),
        (
            "memory",
            json!({"memory": {"stability_days": 10, "difficulty": 11}, "last_review_at": 100}),
        ),
        ("memory", json!(null)),
        ("due_at", json!(99)),
        ("reviews", json!(0)),
        ("lapses", json!(13)),
        ("parameters_id", json!("")),
    ] {
        let mut data = stored_state();
        data[field] = value;
        assert!(serde_json::from_value::<ReviewState>(data).is_err(), "accepted {field}");
    }
}

#[test]
fn reviewed_memory_requires_both_estimate_and_timestamp() {
    for field in ["memory", "last_review_at"] {
        let mut missing = stored_state();
        missing["memory"].as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<ReviewState>(missing).is_err());
        let mut null = stored_state();
        null["memory"][field] = json!(null);
        assert!(serde_json::from_value::<ReviewState>(null).is_err());
    }
    let mut unseen = stored_state();
    unseen["memory"] = json!(null);
    unseen["reviews"] = json!(0);
    unseen["lapses"] = json!(0);
    let state: ReviewState = serde_json::from_value(unseen.clone()).unwrap();
    assert_eq!(serde_json::to_value(state).unwrap(), unseen);
}

#[test]
fn invalid_persisted_configuration_is_rejected() {
    let data = json!({
        "version": "fsrs6_v1", "parameters": vec![0.1_f32; 21],
        "desired_retention": 0.9, "maximum_interval_days": 36500
    });
    for (field, value) in [
        ("version", json!("fsrs5_v1")),
        ("parameters", json!(vec![0.1; 19])),
        ("parameters", json!(vec![0.0; 21])),
        ("desired_retention", json!(1.0)),
        ("maximum_interval_days", json!(0)),
    ] {
        let mut data = data.clone();
        data[field] = value;
        assert!(serde_json::from_value::<FsrsConfig>(data).is_err(), "accepted {field}");
    }
}

#[cfg(feature = "fsrs")]
mod scheduling {
    use super::*;
    use chess_coach::{
        Error,
        review::{FsrsScheduler, MemorySeed, MemoryState, ReviewError, ReviewEvent, ReviewOrigin, ReviewTransition},
    };

    const DAY: i64 = 86_400;

    fn scheduler() -> FsrsScheduler {
        FsrsScheduler::new(FsrsConfig::default()).unwrap()
    }

    fn events() -> Vec<ReviewEvent> {
        vec![
            ReviewEvent {
                at: 100,
                rating: RecallRating::Good,
            },
            ReviewEvent {
                at: 700,
                rating: RecallRating::Again,
            },
            ReviewEvent {
                at: 700 + 2 * DAY,
                rating: RecallRating::Hard,
            },
            ReviewEvent {
                at: 700 + 47 * DAY,
                rating: RecallRating::Easy,
            },
        ]
    }

    fn apply(scheduler: &FsrsScheduler, events: &[ReviewEvent]) -> ReviewState {
        events.iter().fold(scheduler.new_state(0), |state, event| {
            scheduler.review(&state, event.rating, event.at).unwrap().state
        })
    }

    #[test]
    fn all_ratings_match_upstream_and_survive_persistence() {
        let scheduler = scheduler();
        let config = FsrsConfig::default();
        let upstream = fsrs::FSRS::new(&config.parameters).unwrap();
        let mut state = scheduler.new_state(100);
        let mut memory = None;
        for (event, days) in events().iter().zip([0, 0, 2, 45]) {
            let alternatives = upstream.next_states(memory, config.desired_retention, days).unwrap();
            let expected = match event.rating {
                RecallRating::Again => alternatives.again,
                RecallRating::Hard => alternatives.hard,
                RecallRating::Good => alternatives.good,
                RecallRating::Easy => alternatives.easy,
            };
            let transition = scheduler.review(&state, event.rating, event.at).unwrap();
            assert_eq!(transition.elapsed_days, days);
            assert_eq!(transition.interval_days, expected.interval.round().max(1.0) as u32);
            assert_eq!(
                transition.state.due_at,
                event.at + i64::from(transition.interval_days) * DAY
            );
            assert_eq!(
                transition.state.memory.unwrap().memory.stability_days,
                expected.memory.stability
            );
            assert_eq!(
                transition.state.memory.unwrap().memory.difficulty,
                expected.memory.difficulty
            );
            let json = serde_json::to_string(&transition).unwrap();
            let restored: ReviewTransition = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, transition);
            memory = Some(expected.memory);
            state = restored.state;
        }
        assert_eq!((state.reviews, state.lapses), (4, 1));
    }

    #[test]
    fn replay_agrees_with_incremental_updates_and_upstream_history() {
        let scheduler = scheduler();
        let events = events();
        let state = apply(&scheduler, &events);
        let replayed = scheduler.replay(&events, None).unwrap().unwrap();
        assert_eq!(Some(replayed.memory), state.memory.map(|reviewed| reviewed.memory));
        assert_eq!(
            Some(replayed.last_review_at),
            state.memory.map(|reviewed| reviewed.last_review_at)
        );

        let upstream = fsrs::FSRS::default()
            .memory_state(
                fsrs::FSRSItem {
                    reviews: [(3, 0), (1, 0), (2, 2), (4, 45)]
                        .map(|(rating, delta_t)| fsrs::FSRSReview { rating, delta_t })
                        .to_vec(),
                },
                None,
            )
            .unwrap();
        assert_eq!(replayed.memory.stability_days, upstream.stability);
        assert_eq!(replayed.memory.difficulty, upstream.difficulty);

        let prefix = scheduler.replay(&events[..2], None).unwrap();
        assert_eq!(scheduler.replay(&events[2..], prefix).unwrap(), Some(replayed));
    }

    #[test]
    fn timing_uses_elapsed_days_not_midnight_and_changes_scheduling() {
        let scheduler = scheduler();
        let state = scheduler
            .review(&scheduler.new_state(0), RecallRating::Good, DAY - 1)
            .unwrap()
            .state;
        assert_eq!(
            scheduler
                .review(&state, RecallRating::Good, DAY + 1)
                .unwrap()
                .elapsed_days,
            0
        );
        assert_eq!(
            scheduler
                .review(&state, RecallRating::Good, 2 * DAY - 2)
                .unwrap()
                .elapsed_days,
            0
        );
        assert_eq!(
            scheduler
                .review(&state, RecallRating::Good, 2 * DAY - 1)
                .unwrap()
                .elapsed_days,
            1
        );
        let early = scheduler.review(&state, RecallRating::Good, DAY).unwrap();
        let late = scheduler.review(&state, RecallRating::Good, 60 * DAY).unwrap();
        assert!(late.state.memory.unwrap().memory.stability_days > early.state.memory.unwrap().memory.stability_days);
        assert!(scheduler.retrievability(&state, 60 * DAY).unwrap() < scheduler.retrievability(&state, DAY).unwrap());
        assert_eq!(scheduler.retrievability(&scheduler.new_state(0), 0).unwrap(), None);
    }

    #[test]
    fn inconclusive_feedback_never_becomes_a_review() {
        let scheduler = scheduler();
        let mut state = apply(&scheduler, &events());
        let before = state.clone();
        if let Some(rating) = RecallRating::from_grade(Grade::Inconclusive) {
            state = scheduler.review(&state, rating, 100 * DAY).unwrap().state;
        }
        assert_eq!(state, before);
    }

    #[test]
    fn migration_preserves_progress_due_date_and_original_snapshot() {
        let scheduler = scheduler();
        let history = events();
        let previous = LegacyReviewState {
            step: 2,
            reviews: 4,
            lapses: 1,
            due_at: 60 * DAY,
        };
        let state = scheduler.migrate(&previous, &history, None).unwrap();
        assert_eq!(state.memory, apply(&scheduler, &history).memory);
        assert_eq!((state.due_at, state.reviews, state.lapses), (previous.due_at, 4, 1));
        assert_eq!(state.origin, ReviewOrigin::Replayed);
        assert_eq!(state.legacy, Some(previous.clone()));
        let next = scheduler.review(&state, RecallRating::Good, 65 * DAY).unwrap().state;
        assert_eq!((next.reviews, next.lapses), (5, 1));
        assert_eq!(next.legacy, Some(previous));
    }

    #[test]
    fn ambiguous_legacy_states_require_an_explicit_estimate() {
        let scheduler = scheduler();
        for step in [0, 6] {
            let previous = LegacyReviewState {
                step,
                reviews: 12,
                lapses: 2,
                due_at: 40 * DAY,
            };
            assert!(scheduler.migrate(&previous, &[], None).is_err());
            let seed = MemorySeed {
                memory: MemoryState {
                    stability_days: 30.0,
                    difficulty: 5.0,
                },
                last_review_at: 10 * DAY,
            };
            let migrated = scheduler.migrate(&previous, &[], Some(seed)).unwrap();
            assert_eq!(migrated.origin, ReviewOrigin::Estimated);
            assert_eq!(migrated.memory.unwrap().memory, seed.memory);
            assert_eq!(migrated.memory.unwrap().last_review_at, seed.last_review_at);
            assert_eq!((migrated.due_at, migrated.reviews, migrated.lapses), (40 * DAY, 12, 2));
            assert_eq!(migrated.legacy, Some(previous));
        }
        let fresh = scheduler.migrate(&LegacyReviewState::default(), &[], None).unwrap();
        assert_eq!(fresh.memory, None);
        assert_eq!(fresh.reviews, 0);
    }

    #[test]
    fn migration_accepts_a_known_suffix_but_rejects_inconsistent_history() {
        let scheduler = scheduler();
        let history = events();
        let previous = LegacyReviewState {
            step: 2,
            reviews: 4,
            lapses: 1,
            due_at: 60 * DAY,
        };
        assert!(matches!(
            scheduler.migrate(&previous, &history[2..], None),
            Err(Error::Review(ReviewError::InvalidHistory(_)))
        ));
        let seed = scheduler.replay(&history[..2], None).unwrap();
        let migrated = scheduler.migrate(&previous, &history[2..], seed).unwrap();
        assert_eq!(migrated.origin, ReviewOrigin::Estimated);
        assert_eq!(migrated.memory, apply(&scheduler, &history).memory);
        let wrong_lapses = LegacyReviewState {
            lapses: 0,
            ..previous.clone()
        };
        assert!(scheduler.migrate(&wrong_lapses, &history, None).is_err());
        assert!(scheduler.migrate(&wrong_lapses, &history[1..], seed).is_err());
        assert!(scheduler.migrate(&previous, &history, seed).is_err());
        let wrong_due = LegacyReviewState { due_at: 1, ..previous };
        assert!(scheduler.migrate(&wrong_due, &history, None).is_err());
    }

    #[test]
    fn changed_parameters_require_replay_and_retain_due_dates() {
        let original = scheduler();
        let history = events();
        let previous = apply(&original, &history);
        let mut config = original.config().clone();
        config.parameters[0] *= 1.1;
        config.parameters[2] *= 1.2;
        let changed = FsrsScheduler::new(config).unwrap();
        assert!(matches!(
            changed.review(&previous, RecallRating::Good, 100 * DAY),
            Err(Error::Review(ReviewError::ParametersMismatch))
        ));
        let rebuilt = changed.rebuild(&previous, &history, None).unwrap();
        assert_eq!(rebuilt.memory, apply(&changed, &history).memory);
        assert_eq!(
            (rebuilt.due_at, rebuilt.reviews, rebuilt.lapses),
            (previous.due_at, previous.reviews, previous.lapses)
        );
        assert_ne!(rebuilt.parameters_id, previous.parameters_id);
        assert!(changed.review(&rebuilt, RecallRating::Good, 100 * DAY).is_ok());
        let mut wrong_times = history;
        wrong_times.last_mut().unwrap().at += 1;
        assert!(changed.rebuild(&previous, &wrong_times, None).is_err());
    }

    #[test]
    fn retention_and_interval_limits_can_change_without_reinterpreting_memory() {
        let original = scheduler();
        let state = apply(&original, &events());
        let mut config = original.config().clone();
        config.desired_retention = 0.95;
        let higher = FsrsScheduler::new(config.clone()).unwrap();
        assert_eq!(higher.parameters_id(), original.parameters_id());
        assert_ne!(higher.config_id(), original.config_id());
        let standard = original.review(&state, RecallRating::Good, 100 * DAY).unwrap();
        let shorter = higher.review(&state, RecallRating::Good, 100 * DAY).unwrap();
        assert_eq!(shorter.state.memory, standard.state.memory);
        assert_eq!(shorter.state.parameters_id, standard.state.parameters_id);
        assert_ne!(shorter.config_id, standard.config_id);
        assert!(shorter.interval_days < standard.interval_days);
        config.maximum_interval_days = 1;
        let limited = FsrsScheduler::new(config).unwrap();
        assert_eq!(
            limited
                .review(&state, RecallRating::Easy, 100 * DAY)
                .unwrap()
                .interval_days,
            1
        );
    }

    #[test]
    fn invalid_inputs_fail_without_mutating_state() {
        let scheduler = scheduler();
        let state = apply(&scheduler, &events());
        let before = state.clone();
        assert!(matches!(
            scheduler.review(&state, RecallRating::Good, 0),
            Err(Error::Review(ReviewError::NonChronologicalTime))
        ));
        assert!(matches!(
            scheduler.retrievability(&state, 0),
            Err(Error::Review(ReviewError::NonChronologicalTime))
        ));
        assert!(matches!(
            scheduler.review(&scheduler.new_state(0), RecallRating::Good, i64::MAX),
            Err(Error::Review(ReviewError::TimestampOverflow))
        ));
        let mut distant = state.clone();
        distant.memory.as_mut().unwrap().last_review_at = i64::MIN;
        assert!(matches!(
            scheduler.retrievability(&distant, i64::MAX),
            Err(Error::Review(ReviewError::TimestampOverflow))
        ));
        assert!(matches!(
            scheduler.retrievability(&state, i64::MAX),
            Err(Error::Review(ReviewError::TimestampOverflow))
        ));
        let mut history = events();
        history.reverse();
        assert!(scheduler.replay(&history, None).is_err());
        let mut broken = state.clone();
        broken.memory.as_mut().unwrap().memory.stability_days = f32::NAN;
        assert!(matches!(
            scheduler.review(&broken, RecallRating::Good, 100 * DAY),
            Err(Error::Review(ReviewError::InvalidState(_)))
        ));
        let mut config = FsrsConfig::default();
        config.parameters[0] = f32::INFINITY;
        assert!(matches!(
            FsrsScheduler::new(config),
            Err(Error::Review(ReviewError::InvalidConfiguration(_)))
        ));
        assert_eq!(state, before);
    }

    #[cfg(feature = "openapi")]
    #[test]
    fn openapi_exposes_the_versioned_contract() {
        use utoipa::OpenApi;
        #[derive(OpenApi)]
        #[openapi(components(schemas(ReviewState, FsrsConfig, ReviewTransition)))]
        struct Api;
        let value = serde_json::to_value(Api::openapi()).unwrap();
        assert!(
            value["components"]["schemas"]["ChessReviewState"]["properties"]
                .get("step")
                .is_none()
        );
        assert!(
            value["components"]["schemas"]["ChessReviewState"]["properties"]
                .get("parameters_id")
                .is_some()
        );
        assert!(value["components"]["schemas"].get("ChessFsrsConfig").is_some());
        assert!(
            value["components"]["schemas"]["ChessReviewState"]["properties"]
                .get("last_review_at")
                .is_none()
        );
        let reviewed = &value["components"]["schemas"]["ChessReviewedMemory"];
        let required = reviewed["required"].as_array().unwrap();
        assert!(required.contains(&json!("memory")));
        assert!(required.contains(&json!("last_review_at")));
    }
}
