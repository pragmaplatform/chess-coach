use chess_coach::{
    engine::{Evaluation, EvaluationConfidence, EvaluationSource, MoveEvaluation, Score, loss},
    game::{self, Color, Game},
    practice::{self, Exercise, Feedback, Grade, Objective},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    exercise: Exercise,
    submitted_uci: String,
    previous: Pair,
    current: Pair,
}

#[derive(Deserialize)]
struct Pair {
    best: Evaluation,
    played: Evaluation,
}

impl Pair {
    fn comparison(self) -> MoveEvaluation {
        MoveEvaluation::new(self.best, self.played)
    }
}

fn caro_kann() -> Case {
    serde_json::from_str(include_str!("fixtures/practice/caro_kann_qa5.json")).unwrap()
}

fn feedback(mut exercise: Exercise, comparison: MoveEvaluation, assisted: bool) -> Feedback {
    exercise.best = comparison.best;
    exercise.confidence = comparison.confidence;
    let submitted = comparison.played.best_move.clone();
    practice::grade(&exercise, &submitted, comparison.played, assisted).unwrap()
}

#[test]
fn stronger_search_best_move_is_accepted_despite_absolute_score_drift() {
    let case = caro_kann();
    let mut previous = case.previous.comparison();
    previous.played.best_move = case.current.best.best_move.clone();
    previous.played.line = case.current.best.line.clone();
    let current = MoveEvaluation::new(case.current.best.clone(), case.current.best).compared_with(&previous);
    assert_eq!(current.confidence, EvaluationConfidence::Inconclusive);
    for assisted in [false, true] {
        let result = feedback(case.exercise.clone(), current.clone(), assisted);
        assert_eq!(result.grade, if assisted { Grade::Assisted } else { Grade::Correct });
        assert_eq!(result.objective_met, Some(true));
        assert_eq!(result.confidence, EvaluationConfidence::Unverified);
    }
}

#[test]
fn caro_kann_qa5_recovers_good_when_the_preferred_move_changes_but_the_answer_stays_stable() {
    let case = caro_kann();
    assert_eq!(case.submitted_uci, "d8a5");
    let previous = case.previous.comparison();
    let current = case.current.comparison();
    assert_eq!(previous.best.best_move, "f5e4");
    assert_eq!(current.best.best_move, "b8d7");
    assert!(loss(previous.best.score, previous.played.score) > 0.05);
    let strict = current.clone().compared_with(&previous);
    assert_eq!(strict.confidence, EvaluationConfidence::Inconclusive);
    assert_eq!(
        feedback(case.exercise.clone(), strict, false).grade,
        Grade::Inconclusive
    );
    let verified = practice::verify_comparison(&case.exercise, current, &previous);
    for assisted in [false, true] {
        let result = feedback(case.exercise.clone(), verified.clone(), assisted);
        assert_eq!(result.grade, if assisted { Grade::Assisted } else { Grade::Good });
        assert_eq!(result.confidence, EvaluationConfidence::Unverified);
        assert_eq!(result.objective_met, Some(true));
        assert!((result.loss - 0.026673648767552904).abs() < 1e-12);
        assert_ne!(
            result.teaching.as_ref().unwrap().explanation,
            "The evaluations have not settled enough to judge this move."
        );
        assert!(practice::follow_up(&case.exercise.position, &result).unwrap().is_some());
    }
}

#[test]
fn acceptable_alternatives_survive_shared_score_drift() {
    for (before_played, after_played, expected) in [(-30, 70, Grade::Good), (-5, 95, Grade::Correct)] {
        let case = caro_kann();
        let mut previous = case.previous.comparison();
        let mut current = case.current.comparison();
        previous.best.score = Score::Centipawns(0);
        previous.played.score = Score::Centipawns(before_played);
        current.best.score = Score::Centipawns(100);
        current.played.score = Score::Centipawns(after_played);
        assert_eq!(
            current.clone().compared_with(&previous).confidence,
            EvaluationConfidence::Inconclusive
        );
        let verified = practice::verify_comparison(&case.exercise, current, &previous);
        assert_eq!(feedback(case.exercise, verified, false).grade, expected);
    }
}

#[test]
fn recovery_keeps_ambiguous_contradictory_and_unverified_alternatives_inconclusive() {
    for reason in [
        "near_cutoff",
        "clear_previous_failure",
        "unstable_answer",
        "unstable_loss",
        "inverted_current",
        "inverted_previous",
        "mixed_sources",
        "different_source",
        "same_budget",
        "weaker_played",
        "different_move",
        "changed_mate",
    ] {
        let case = caro_kann();
        let mut previous = case.previous.comparison();
        let mut current = case.current.comparison();
        match reason {
            "near_cutoff" => current.played.score = Score::Centipawns(-35),
            "clear_previous_failure" => previous.best.score = Score::Centipawns(100),
            "unstable_answer" => {
                previous.best.score = Score::Centipawns(-45);
                previous.played.score = Score::Centipawns(-100);
            }
            "unstable_loss" => {
                previous.best.score = Score::Centipawns(100);
                previous.played.score = Score::Centipawns(100);
            }
            "inverted_current" => current.played.score = Score::Centipawns(100),
            "inverted_previous" => previous.best.score = Score::Centipawns(-40),
            "mixed_sources" => current.played.source = EvaluationSource::LichessCloud,
            "different_source" => {
                previous.best.source = EvaluationSource::LichessCloud;
                previous.played.source = EvaluationSource::LichessCloud;
            }
            "same_budget" => current.best.nodes = previous.best.nodes,
            "weaker_played" => current.played.nodes = previous.played.nodes - 1,
            "different_move" => previous.played.best_move = "b8d7".into(),
            "changed_mate" => {
                previous.best.score = Score::Mate(5);
                previous.played.score = Score::Mate(5);
                current.best.score = Score::Centipawns(800);
                current.played.score = Score::Centipawns(799);
            }
            _ => unreachable!(),
        }
        assert_eq!(
            practice::verify_comparison(&case.exercise, current, &previous).confidence,
            EvaluationConfidence::Inconclusive,
            "{reason}"
        );
    }
}

#[test]
fn best_move_recovery_still_requires_a_supported_mating_objective() {
    for (objective, score) in [
        (Objective::FindMate, Score::Centipawns(800)),
        (Objective::AvoidMate, Score::Mate(-3)),
    ] {
        let case = caro_kann();
        let mut exercise = case.exercise;
        exercise.objective = objective;
        let mut best = case.current.best;
        best.score = score;
        let mut current = MoveEvaluation::new(best.clone(), best);
        current.confidence = EvaluationConfidence::Inconclusive;
        for assisted in [false, true] {
            let result = feedback(exercise.clone(), current.clone(), assisted);
            assert_eq!(result.grade, Grade::Inconclusive);
            assert_eq!(result.objective_met, None);
            assert!(practice::follow_up(&exercise.position, &result).unwrap().is_none());
        }
    }
}

#[test]
fn same_move_name_does_not_override_disagreeing_evaluations() {
    let case = caro_kann();
    let mut current = MoveEvaluation::new(case.current.best.clone(), case.current.best);
    current.played.score = Score::Centipawns(13);
    current.confidence = EvaluationConfidence::Inconclusive;
    assert_eq!(feedback(case.exercise, current, false).grade, Grade::Inconclusive);
}

#[test]
fn recovery_uses_the_exercises_legacy_and_opening_cutoffs() {
    let mut case = caro_kann();
    case.exercise.minimum_loss = None;
    let current = practice::verify_comparison(&case.exercise, case.current.comparison(), &case.previous.comparison());
    assert_eq!(current.confidence, EvaluationConfidence::Inconclusive);

    let case = caro_kann();
    let mut previous = case.previous.comparison();
    previous.best.score = Score::Centipawns(70);
    previous.played.score = Score::Centipawns(0);
    let mut current = case.current.comparison();
    current.best.score = Score::Centipawns(100);
    current.played.score = Score::Centipawns(30);
    assert_eq!(
        practice::verify_comparison(&case.exercise, current.clone(), &previous).confidence,
        EvaluationConfidence::Inconclusive
    );
    let mut exercise = case.exercise;
    exercise.position = game::decisions(
        &Game {
            id: "early-cutoff".into(),
            player_color: Color::Black,
            moves_san: "e4 c6 d4 d5 Nc3 dxe4 Nxe4 Bf5 f3 Qa5+".into(),
        },
        15,
    )
    .unwrap()
    .pop()
    .unwrap()
    .position;
    previous.best.line.truncate(1);
    previous.played.line.truncate(1);
    current.best.line.truncate(1);
    current.played.line.truncate(1);
    let current = practice::verify_comparison(&exercise, current, &previous);
    assert_eq!(feedback(exercise, current, false).grade, Grade::Good);
}
