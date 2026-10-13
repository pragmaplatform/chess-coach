use serde::{Deserialize, Serialize};

use crate::practice::Grade;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessReviewState))]
pub struct ReviewState {
    pub step: u8,
    pub reviews: u32,
    pub lapses: u32,
    pub due_at: i64,
}

/// Four correct answers in a row space the next review at least two weeks out.
pub const LEARNED_STEP: u8 = 4;

impl ReviewState {
    pub fn learned(&self) -> bool {
        self.step >= LEARNED_STEP
    }
}

pub fn schedule(previous: &ReviewState, grade: Grade, now: i64) -> ReviewState {
    const DAYS: [i64; 7] = [1, 3, 7, 14, 30, 60, 120];
    let (step, delay) = match grade {
        Grade::Correct | Grade::Good => {
            let index = usize::from(previous.step).min(DAYS.len() - 1);
            ((index + 1).min(DAYS.len() - 1) as u8, DAYS[index] * 86_400)
        }
        Grade::Incorrect => (0, 600),
        Grade::Assisted => (0, 3600),
        Grade::Inconclusive => return previous.clone(),
    };
    ReviewState {
        step,
        reviews: previous.reviews.saturating_add(1),
        lapses: previous
            .lapses
            .saturating_add(u32::from(matches!(grade, Grade::Incorrect | Grade::Assisted))),
        due_at: now.saturating_add(delay),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn success_advances_and_failure_relearns() {
        let initial = ReviewState::default();
        let first = schedule(&initial, Grade::Correct, 100);
        assert_eq!(first.due_at, 100 + 86400);
        let second = schedule(&first, Grade::Correct, first.due_at);
        assert_eq!(second.due_at, first.due_at + 3 * 86400);
        let failed = schedule(&second, Grade::Incorrect, second.due_at);
        assert_eq!(failed.step, 0);
        assert_eq!(failed.lapses, 1);
        assert_eq!(failed.due_at, second.due_at + 600);
    }

    #[test]
    fn good_answers_advance_without_adding_lapses() {
        let previous = ReviewState {
            step: 2,
            reviews: 4,
            lapses: 1,
            due_at: 100,
        };
        let next = schedule(&previous, Grade::Good, 100);
        assert_eq!(next.step, 3);
        assert_eq!(next.reviews, 5);
        assert_eq!(next.lapses, 1);
        assert_eq!(next.due_at, 100 + 7 * 86_400);
        let assisted = schedule(&next, Grade::Assisted, next.due_at);
        assert_eq!(assisted.step, 0);
        assert_eq!(assisted.lapses, 2);
        assert_eq!(assisted.due_at, next.due_at + 3600);
    }
}
