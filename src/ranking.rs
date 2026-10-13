use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    analysis::{Observation, Severity},
    engine::EvaluationConfidence,
    game::Color,
    teaching::Theme,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessWeakness))]
pub struct Weakness {
    pub position_key: String,
    pub played_uci: String,
    pub color: Color,
    pub occurrences: u32,
    pub reached_games: u32,
    pub average_loss: f64,
    /// Null below five percentage points or for weaknesses saved before severity labels.
    #[serde(default)]
    pub severity: Option<Severity>,
    pub priority: f64,
    pub example: Observation,
    #[serde(default)]
    pub themes: Vec<Theme>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessThemeSummary))]
pub struct ThemeSummary {
    pub theme: Theme,
    pub positions: u32,
    pub games: u32,
    pub occurrences: u32,
    pub average_loss: f64,
    pub priority: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "openapi", schema(as = ChessReport))]
pub struct Report {
    pub games_analyzed: u32,
    pub weaknesses: Vec<Weakness>,
    #[serde(default)]
    pub themes: Vec<ThemeSummary>,
}

type MistakeGroups<'a> = BTreeMap<(&'a str, &'a str), BTreeMap<&'a str, &'a Observation>>;

pub fn aggregate(observations: &[Observation], games_analyzed: u32) -> Report {
    let mut reached: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for observation in observations {
        let decision = &observation.decision;
        reached
            .entry(&decision.position.key)
            .or_default()
            .insert(&decision.game_id);
    }
    let groups = mistake_groups(observations);
    let mut weaknesses: Vec<_> = groups
        .into_iter()
        .filter_map(|((position, played), examples)| {
            let example = examples
                .values()
                .max_by(|a, b| example_quality(a).cmp(&example_quality(b)))?
                .to_owned()
                .clone();
            let sum: f64 = examples.values().map(|o| o.loss).sum();
            let count = examples.len() as u32;
            let average_loss = sum / f64::from(count);
            Some(Weakness {
                position_key: position.into(),
                played_uci: played.into(),
                color: example.decision.color,
                occurrences: count,
                reached_games: reached.get(position).map_or(0, |g| g.len() as u32),
                average_loss,
                severity: Severity::from_loss(average_loss),
                priority: sum / f64::from(games_analyzed.max(1)),
                example,
                themes: examples
                    .values()
                    .filter_map(|o| o.teaching.as_ref())
                    .flat_map(|teaching| teaching.themes.iter().copied())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            })
        })
        .collect();
    weaknesses.sort_by(|a, b| {
        b.priority
            .total_cmp(&a.priority)
            .then_with(|| a.position_key.cmp(&b.position_key))
            .then_with(|| a.played_uci.cmp(&b.played_uci))
    });
    Report {
        games_analyzed,
        weaknesses,
        themes: aggregate_themes(observations, games_analyzed),
    }
}

fn mistake_groups<'a>(observations: impl IntoIterator<Item = &'a Observation>) -> MistakeGroups<'a> {
    let mut groups = MistakeGroups::new();
    for observation in observations {
        if observation.mistake && observation.confidence != EvaluationConfidence::Inconclusive {
            let decision = &observation.decision;
            let examples = groups
                .entry((&decision.position.key, &decision.played_uci))
                .or_default();
            let first = examples.entry(&decision.game_id).or_insert(observation);
            if decision.ply < first.decision.ply {
                *first = observation;
            }
        }
    }
    groups
}

fn example_quality(observation: &Observation) -> (EvaluationConfidence, bool, u16, std::cmp::Reverse<usize>) {
    (
        observation.confidence,
        observation
            .teaching
            .as_ref()
            .is_some_and(|teaching| !teaching.themes.is_empty()),
        observation.best.depth.min(observation.played.depth),
        std::cmp::Reverse(observation.best.line.len().max(observation.played.line.len())),
    )
}

fn aggregate_themes(observations: &[Observation], games_analyzed: u32) -> Vec<ThemeSummary> {
    let mut groups: BTreeMap<Theme, BTreeMap<(&str, &str), &Observation>> = BTreeMap::new();
    for observation in observations {
        if !observation.mistake || observation.confidence == EvaluationConfidence::Inconclusive {
            continue;
        }
        if let Some(teaching) = &observation.teaching {
            for &theme in &teaching.themes {
                let first = groups
                    .entry(theme)
                    .or_default()
                    .entry((&observation.decision.game_id, &observation.decision.position.key))
                    .or_insert(observation);
                if observation.decision.ply < first.decision.ply {
                    *first = observation;
                }
            }
        }
    }
    let mut themes: Vec<_> = groups
        .into_iter()
        .map(|(theme, examples)| {
            let sum: f64 = examples.values().map(|o| o.loss).sum();
            ThemeSummary {
                theme,
                positions: examples
                    .keys()
                    .map(|(_, position)| position)
                    .collect::<BTreeSet<_>>()
                    .len() as u32,
                games: examples.keys().map(|(game, _)| game).collect::<BTreeSet<_>>().len() as u32,
                occurrences: examples.len() as u32,
                average_loss: sum / examples.len() as f64,
                priority: sum / f64::from(games_analyzed.max(1)),
            }
        })
        .collect();
    themes.sort_by(|a, b| b.priority.total_cmp(&a.priority).then_with(|| a.theme.cmp(&b.theme)));
    themes
}
