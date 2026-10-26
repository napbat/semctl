//! The output of a command that prints a query answer.
//!
//! The answer goes to stdout. A warning for each partial graph answer goes
//! straight to stderr, not through the log, so a log filter (`RUST_LOG`)
//! cannot hide that the answer is not complete.

use std::io::{self, Write};

use anyhow::{Context, Result};

use crate::client::{PartialAnswer, PartialAnswers};

/// Print `answer` on stdout with one final newline, then a warning on stderr
/// for each partial graph answer in `partial`.
pub(crate) fn print(answer: &str, partial: &PartialAnswers) -> Result<()> {
    write(
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        answer,
        &partial.answers(),
    )
    .context("write the answer")
}

fn write(
    out: &mut impl Write,
    err: &mut impl Write,
    answer: &str,
    partial: &[PartialAnswer],
) -> io::Result<()> {
    out.write_all(answer.as_bytes())?;
    if !answer.ends_with('\n') {
        out.write_all(b"\n")?;
    }
    // The answer comes first, so the warnings follow it on a terminal.
    out.flush()?;
    for answer in partial {
        writeln!(err, "warning: {answer}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write;
    use crate::client::stub::{Reply, Stub};
    use crate::client::{Client, PartialAnswers};

    /// The stdout and stderr text of `answer` after a call whose server sent
    /// `replies`, one request for each reply.
    async fn output(answer: &str, replies: Vec<Reply>) -> (String, String) {
        let count = replies.len();
        let stub = Stub::serve(replies).await;
        let partial = PartialAnswers::default();
        let client = Client::for_test_server(&stub.url).with_partial_answers(partial.clone());
        for _ in 0..count {
            client
                .get::<serde_json::Value>("/v1/codebases/c/graph/references")
                .await
                .expect("the answer is a success");
        }
        let (mut out, mut err) = (Vec::new(), Vec::new());
        write(&mut out, &mut err, answer, &partial.answers()).expect("write to memory");
        (
            String::from_utf8(out).expect("UTF-8 output"),
            String::from_utf8(err).expect("UTF-8 output"),
        )
    }

    #[tokio::test]
    async fn a_complete_answer_prints_no_warning() {
        let (out, err) = output("a.rs:1", vec![Reply::ok("{}")]).await;

        assert_eq!(out, "a.rs:1\n");
        assert_eq!(err, "");
    }

    #[tokio::test]
    async fn each_partial_answer_prints_one_warning_on_stderr_after_the_answer() {
        let (out, err) = output(
            "a.rs:1\n",
            vec![
                Reply::partial("providers-unavailable", "none", "{}"),
                Reply::partial("flow-truncated", "none", "{}"),
                Reply::partial("providers-loading", "pending", "{}"),
                Reply::partial("flow-truncated", "none", "{}"),
            ],
        )
        .await;

        assert_eq!(out, "a.rs:1\n", "the answer stays as the query gave it");
        assert_eq!(
            err,
            "warning: the graph answer is partial (providers-unavailable); a retry returns the \
             same result\n\
             warning: the graph answer is partial (flow-truncated); a retry returns the same \
             result\n\
             warning: the graph answer is partial (providers-loading); the server still builds \
             the missing part, so a later call can return more\n"
        );
    }
}
