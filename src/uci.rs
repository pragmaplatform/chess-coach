use std::{path::Path, process::Stdio, time::Duration};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

use crate::{
    Error,
    engine::{Engine, Evaluation, EvaluationRequest, EvaluationSource, Score},
    game,
};

pub struct Stockfish {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
    name: String,
}

impl Stockfish {
    pub async fn spawn(path: impl AsRef<Path>) -> Result<Self, Error> {
        let mut child = Command::new(path.as_ref())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| Error::Engine("Missing engine stdin".into()))?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| Error::Engine("Missing engine stdout".into()))?,
        )
        .lines();
        let mut engine = Self {
            child,
            input,
            output,
            name: String::new(),
        };
        timeout(Duration::from_secs(15), async {
            engine.command("uci").await?;
            loop {
                let line = engine.line().await?;
                if let Some(name) = line.strip_prefix("id name ") {
                    engine.name = name.into();
                }
                if line == "uciok" {
                    return Ok::<(), Error>(());
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout)??;
        Ok(engine)
    }

    async fn command(&mut self, command: &str) -> Result<(), Error> {
        self.input.write_all(format!("{command}\n").as_bytes()).await?;
        self.input.flush().await?;
        Ok(())
    }

    async fn line(&mut self) -> Result<String, Error> {
        self.output
            .next_line()
            .await?
            .ok_or_else(|| Error::Engine("Engine exited before completing the search".into()))
    }

    async fn search(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
        request.profile.validate()?;
        if self.name != request.profile.engine_name {
            return Err(Error::Configuration(format!(
                "Expected {}, found {}",
                request.profile.engine_name, self.name
            )));
        }
        let board = game::board(&request.position)?;
        if let Some(mv) = &request.root_move {
            game::legal_move(&board, mv)?;
        }
        self.command(&format!("setoption name Threads value {}", request.profile.threads))
            .await?;
        self.command(&format!("setoption name Hash value {}", request.profile.hash_mb))
            .await?;
        self.command("ucinewgame").await?;
        self.command("isready").await?;
        while self.line().await? != "readyok" {}
        self.command(&format!(
            "position startpos moves {}",
            request.position.history_uci.join(" ")
        ))
        .await?;
        let restriction = request
            .root_move
            .as_ref()
            .map(|mv| format!(" searchmoves {mv}"))
            .unwrap_or_default();
        self.command(&format!("go nodes {}{restriction}", request.profile.nodes))
            .await?;
        let mut evaluation = None;
        loop {
            let line = self.line().await?;
            if line.starts_with("bestmove ") {
                break;
            }
            if let Some(parsed) = parse_info(&line) {
                evaluation = Some(parsed);
            }
        }
        let result = evaluation.ok_or_else(|| Error::Engine("No completed exact evaluation".into()))?;
        game::line_san(&request.position, &result.line)?;
        if request.root_move.as_ref().is_some_and(|mv| *mv != result.best_move) {
            return Err(Error::Engine("Engine ignored root move restriction".into()));
        }
        Ok(result)
    }
}

#[async_trait::async_trait]
impl Engine for Stockfish {
    async fn evaluate(&mut self, request: &EvaluationRequest) -> Result<Evaluation, Error> {
        match timeout(Duration::from_millis(request.profile.timeout_ms), self.search(request)).await {
            Ok(result) => result,
            Err(_) => {
                self.child.kill().await?;
                Err(Error::Timeout)
            }
        }
    }
}

fn parse_info(line: &str) -> Option<Evaluation> {
    let tokens: Vec<_> = line.split_whitespace().collect();
    if tokens.first() != Some(&"info") || tokens.contains(&"lowerbound") || tokens.contains(&"upperbound") {
        return None;
    }
    let value = |key| {
        tokens
            .iter()
            .position(|v| *v == key)
            .and_then(|i| tokens.get(i + 1))
            .copied()
    };
    let score_index = tokens.iter().position(|v| *v == "score")?;
    let score = match *tokens.get(score_index + 1)? {
        "cp" => Score::Centipawns(tokens.get(score_index + 2)?.parse().ok()?),
        "mate" => Score::Mate(tokens.get(score_index + 2)?.parse().ok()?),
        _ => return None,
    };
    let pv = tokens.iter().position(|v| *v == "pv")?;
    let line: Vec<String> = tokens[pv + 1..].iter().take(16).map(|s| (*s).into()).collect();
    Some(Evaluation {
        source: EvaluationSource::Stockfish,
        score,
        best_move: line.first()?.clone(),
        line,
        depth: value("depth")?.parse().ok()?,
        nodes: value("nodes")?.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_exact_scores_and_rejects_bounds() {
        let score = parse_info("info depth 12 score cp -52 nodes 999 pv e2e4 e7e5").unwrap();
        assert_eq!(score.score, Score::Centipawns(-52));
        assert_eq!(score.best_move, "e2e4");
        assert!(parse_info("info depth 12 score cp -52 upperbound nodes 999 pv e2e4").is_none());
        assert_eq!(
            parse_info("info depth 6 score mate -3 nodes 555 pv e2e4")
                .unwrap()
                .score,
            Score::Mate(-3)
        );
    }
}
