//! The output of a command that prints a query answer.
//!
//! The answer goes to stdout. A warning for each partial graph answer goes
//! straight to stderr, not through the log, so a log filter (`RUST_LOG`)
//! cannot hide that the answer is not complete.
//! Native JSON also carries the warnings separately from the text and display.

use std::io::{self, Write};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::client::{PartialAnswer, PartialAnswers};
use crate::query::QueryOutput;

/// Print `answer` on stdout with one final newline, then a warning on stderr
/// for each partial graph answer in `partial`.
pub(crate) fn print(answer: &str, partial: &PartialAnswers) -> Result<()> {
    write(
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        answer,
        &partial.take(),
    )
    .context("write the answer")
}

/// Print the native JSON answer on stdout and its warnings on stderr.
pub(crate) fn print_json(output: &QueryOutput, partial: &PartialAnswers) -> Result<()> {
    write_json(
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        output,
        &partial.take(),
    )
    .context("write the answer")
}

#[derive(Serialize)]
struct JsonAnswer<'a> {
    #[serde(flatten)]
    output: &'a QueryOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

fn write_json(
    out: &mut impl Write,
    err: &mut impl Write,
    output: &QueryOutput,
    partial: &[PartialAnswer],
) -> Result<()> {
    let answer = JsonAnswer {
        output,
        warnings: partial.iter().map(ToString::to_string).collect(),
    };
    serde_json::to_writer(&mut *out, &answer)?;
    out.write_all(b"\n")?;
    out.flush()?;
    write_warnings(err, &answer.warnings)?;
    Ok(())
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
    write_warnings(err, partial)
}

fn write_warnings(
    err: &mut impl Write,
    partial: impl IntoIterator<Item = impl std::fmt::Display>,
) -> io::Result<()> {
    for answer in partial {
        writeln!(err, "warning: {answer}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{QueryOutput, write, write_json};
    use crate::client::stub::{Reply, Stub};
    use crate::client::{Client, PartialAnswers};

    /// Send one request per reply and collect the call's partial answers.
    async fn partial_answers(replies: Vec<Reply>) -> PartialAnswers {
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
        partial
    }

    async fn output(answer: &str, replies: Vec<Reply>) -> (String, String) {
        let partial = partial_answers(replies).await;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        write(&mut out, &mut err, answer, &partial.take()).expect("write to memory");
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

    #[tokio::test]
    async fn native_json_keeps_the_query_output_and_carries_warnings_separately() {
        let output = QueryOutput {
            text: "a.rs:1\n".to_string(),
            display: Some(json!({"version": 1, "kind": "graph"})),
        };
        let complete = serde_json::to_value(&output).unwrap();
        let warning =
            "the graph answer is partial (flow-truncated); a retry returns the same result";
        for (reply, expected) in [
            (Reply::ok("{}"), complete.clone()),
            (
                Reply::partial("flow-truncated", "none", "{}"),
                json!({
                    "text": complete["text"], "display": complete["display"],
                    "warnings": [warning]
                }),
            ),
        ] {
            let partial = partial_answers(vec![reply]).await;
            let (mut out, mut err) = (Vec::new(), Vec::new());
            write_json(&mut out, &mut err, &output, &partial.take()).expect("write to memory");
            assert!(out.ends_with(b"\n"));
            let actual = serde_json::from_slice::<Value>(&out).unwrap();
            assert_eq!(actual, expected);
            if actual.get("warnings").is_some() {
                assert_eq!(err, format!("warning: {warning}\n").into_bytes());
            } else {
                assert_eq!(err.as_slice(), b"");
            }
        }
    }
}
