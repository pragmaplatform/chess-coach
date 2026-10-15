use chess_coach::{
    engine::{Evaluation, EvaluationConfidence, EvaluationSource, MoveEvaluation, Score},
    practice::{self, Exercise, Grade, Objective},
    review::RecallRating,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    exercise: Exercise,
    submitted_uci: String,
    played: Evaluation,
}

fn caro_kann() -> Case {
    serde_json::from_str(include_str!("fixtures/practice/caro_kann_fxe5.json")).unwrap()
}

#[test]
fn legacy_assisted_feedback_is_readable_but_has_no_independent_verdict() {
    let case = caro_kann();
    let result = practice::grade(&case.exercise, &case.submitted_uci, case.played).unwrap();
    let mut json = serde_json::to_value(&result).unwrap();
    json["grade"] = "assisted".into();
    let legacy: practice::Feedback = serde_json::from_value(json).unwrap();
    assert_eq!(legacy.grade, Grade::Inconclusive);
    assert_eq!(legacy.best, result.best);
    assert_eq!(legacy.played, result.played);
    assert_eq!(RecallRating::from_grade(legacy.grade), None);
    assert!(practice::follow_up(&case.exercise.position, &legacy).unwrap().is_none());
    assert_eq!(serde_json::to_value(legacy).unwrap()["grade"], "inconclusive");
}

#[test]
fn caro_kann_fxe5_avoids_mate_despite_outscoring_the_reference() {
    let case = caro_kann();
    assert_eq!(case.exercise.objective, Objective::AvoidMate);
    assert_eq!(case.exercise.best.score, Score::Centipawns(661));
    assert_eq!(case.played.score, Score::Centipawns(734));
    assert_eq!(case.submitted_uci, "f6e5");
    assert_eq!(
        MoveEvaluation::new(case.exercise.best.clone(), case.played.clone()).confidence,
        EvaluationConfidence::Inconclusive
    );
    let result = practice::grade(&case.exercise, &case.submitted_uci, case.played.clone()).unwrap();
    assert_eq!(result.grade, Grade::Correct);
    assert_eq!(result.confidence, EvaluationConfidence::Unverified);
    assert_eq!(result.objective_met, Some(true));
    assert_eq!(result.loss, 0.0);
    assert_eq!(result.best, case.exercise.best);
    assert_eq!(result.played, case.played);
    assert_ne!(
        result.teaching.as_ref().unwrap().explanation,
        "The evaluations have not settled enough to judge this move."
    );
    assert_eq!(RecallRating::from_grade(result.grade), Some(RecallRating::Good));
    let child = practice::follow_up(&case.exercise.position, &result).unwrap().unwrap();
    assert_eq!(child.setup_san, ["fxe5", "g4"]);
    let json = serde_json::to_value(result).unwrap();
    assert_eq!(json["confidence"], "unverified");
    assert_eq!(json["grade"], "correct");
}

#[test]
fn winning_inversion_does_not_override_other_objectives_or_uncertain_evidence() {
    for reason in [
        "best_move",
        "find_mate",
        "uncertain_exercise",
        "cloud",
        "not_clearly_winning",
        "losing",
    ] {
        let mut case = caro_kann();
        match reason {
            "best_move" => case.exercise.objective = Objective::BestMove,
            "find_mate" => case.exercise.objective = Objective::FindMate,
            "uncertain_exercise" => case.exercise.confidence = EvaluationConfidence::Inconclusive,
            "cloud" => {
                case.exercise.best.source = EvaluationSource::LichessCloud;
                case.played.source = EvaluationSource::LichessCloud;
            }
            "not_clearly_winning" => case.exercise.best.score = Score::Centipawns(597),
            "losing" => {
                case.exercise.best.score = Score::Centipawns(-734);
                case.played.score = Score::Centipawns(-661);
            }
            _ => unreachable!(),
        }
        let result = practice::grade(&case.exercise, &case.submitted_uci, case.played.clone()).unwrap();
        assert_eq!(result.grade, Grade::Inconclusive, "{reason}");
        assert_eq!(result.objective_met, None, "{reason}");
        assert_eq!(RecallRating::from_grade(result.grade), None);
    }
    let mut case = caro_kann();
    case.exercise.best.score = Score::Centipawns(598);
    assert_eq!(
        practice::grade(&case.exercise, &case.submitted_uci, case.played)
            .unwrap()
            .grade,
        Grade::Correct
    );
}

#[test]
fn winning_inversion_does_not_hide_mate_contradictions_or_a_failed_answer() {
    for (best, played, expected) in [
        (Score::Mate(-2), Score::Centipawns(734), Grade::Inconclusive),
        (Score::Centipawns(661), Score::Mate(2), Grade::Inconclusive),
        (Score::Centipawns(661), Score::Mate(-2), Grade::Incorrect),
        (Score::Centipawns(661), Score::Centipawns(-734), Grade::Incorrect),
    ] {
        let mut case = caro_kann();
        case.exercise.best.score = best;
        case.played.score = played;
        let result = practice::grade(&case.exercise, &case.submitted_uci, case.played).unwrap();
        assert_eq!(result.grade, expected);
    }
}

#[test]
fn winning_inversion_preserves_source_and_move_validation() {
    let mut case = caro_kann();
    case.played.source = EvaluationSource::LichessCloud;
    assert!(practice::grade(&case.exercise, &case.submitted_uci, case.played).is_err());

    let mut case = caro_kann();
    case.played.best_move = "e7e6".into();
    assert!(practice::grade(&case.exercise, &case.submitted_uci, case.played).is_err());

    let mut case = caro_kann();
    case.played.best_move = "f6f4".into();
    assert!(practice::grade(&case.exercise, "f6f4", case.played).is_err());
}

#[test]
fn winning_inversion_does_not_bypass_a_verifications_changed_mating_outcome() {
    let mut case = caro_kann();
    let current = MoveEvaluation::new(case.exercise.best.clone(), case.played.clone());
    let mut previous = current.clone();
    previous.best.score = Score::Mate(3);
    previous.played.score = Score::Mate(4);
    previous.best.nodes /= 5;
    previous.played.nodes /= 5;
    let verified = practice::verify_comparison(&case.exercise, current, &previous);
    assert_eq!(verified.confidence, EvaluationConfidence::Inconclusive);
    case.exercise.best = verified.best;
    case.exercise.confidence = verified.confidence;
    let result = practice::grade(&case.exercise, &case.submitted_uci, verified.played).unwrap();
    assert_eq!(result.grade, Grade::Inconclusive);
    assert_eq!(result.objective_met, None);
    assert_eq!(RecallRating::from_grade(result.grade), None);
    assert!(practice::follow_up(&case.exercise.position, &result).unwrap().is_none());
}

#[test]
fn evidence_preserves_the_saved_exercise_across_raw_and_verified_searches() {
    use practice::Evidence;
    let case = caro_kann();
    let current = MoveEvaluation::new(case.exercise.best.clone(), case.played.clone());
    let mut previous = current.clone();
    previous.best.nodes /= 5;
    previous.played.nodes /= 5;
    for evidence in [
        Evidence::Search(current.clone()),
        Evidence::Verification { current, previous },
    ] {
        let assessment = practice::assess(&case.exercise, evidence.clone());
        assert_eq!(assessment.uncertainty, None);
        let result = practice::grade_with_evidence(&case.exercise, &case.submitted_uci, evidence.clone()).unwrap();
        assert_eq!(result.grade, Grade::Correct);
        assert_eq!(result.confidence, EvaluationConfidence::Unverified);
    }
    assert_eq!(case.exercise.confidence, EvaluationConfidence::Confirmed);
}

#[test]
fn evidence_distinguishes_ordering_drift_mates_and_invalid_comparisons() {
    use practice::{Evidence, Uncertainty};
    for reason in [
        Uncertainty::ScoreOrdering,
        Uncertainty::ScoreDrift,
        Uncertainty::MateContradiction,
        Uncertainty::MateChanged,
        Uncertainty::SourceMismatch,
        Uncertainty::SubmittedMoveMismatch,
        Uncertainty::UnsupportedObjective,
        Uncertainty::Unspecified,
    ] {
        let mut case = caro_kann();
        let mut current = MoveEvaluation::new(case.exercise.best.clone(), case.played.clone());
        let mut previous = current.clone();
        previous.best.nodes /= 5;
        previous.played.nodes /= 5;
        let evidence = match reason {
            Uncertainty::ScoreOrdering => {
                case.exercise.objective = Objective::BestMove;
                Evidence::Search(current)
            }
            Uncertainty::ScoreDrift => {
                previous.best.score = Score::Centipawns(0);
                previous.played.score = Score::Centipawns(-200);
                Evidence::Verification { current, previous }
            }
            Uncertainty::MateContradiction => {
                current.played.score = Score::Mate(3);
                Evidence::Search(current)
            }
            Uncertainty::MateChanged => {
                previous.best.score = Score::Mate(3);
                previous.played.score = Score::Mate(4);
                // Even the exact current root best must not erase a mate change.
                current.best = current.played.clone();
                Evidence::Verification { current, previous }
            }
            Uncertainty::SourceMismatch => {
                previous.best.source = EvaluationSource::LichessCloud;
                previous.played.source = EvaluationSource::LichessCloud;
                Evidence::Verification { current, previous }
            }
            Uncertainty::SubmittedMoveMismatch => {
                previous.played.best_move = "e7e6".into();
                Evidence::Verification { current, previous }
            }
            Uncertainty::UnsupportedObjective => {
                case.exercise.objective = Objective::FindMate;
                Evidence::Search(current)
            }
            Uncertainty::Unspecified => {
                current.played.score = current.best.score;
                current.confidence = EvaluationConfidence::Inconclusive;
                Evidence::Search(current)
            }
        };
        let assessment = practice::assess(&case.exercise, evidence.clone());
        assert_eq!(assessment.uncertainty, Some(reason));
        let result = practice::grade_with_evidence(&case.exercise, &case.submitted_uci, evidence).unwrap();
        assert_eq!(result.grade, Grade::Inconclusive, "{reason:?}");
        assert_eq!(RecallRating::from_grade(result.grade), None);
    }
}
