use std::time::Duration;

use serde_json::Value;
use tokio::io::{BufReader, Lines};
use tokio::process::ChildStdout;

/// Notifications can interleave responses. One deadline bounds the entire wait;
/// callers still check response IDs, including concurrent request ordering.
pub async fn response(
    stdout: &mut Lines<BufReader<ChildStdout>>,
    timeout: Duration,
    what: &str,
) -> (String, Value) {
    tokio::time::timeout(timeout, async {
        loop {
            let line = stdout
                .next_line()
                .await
                .expect("read MCP output")
                .expect("MCP server closed stdout before responding");
            let message: Value = serde_json::from_str(&line).expect("parse MCP message");
            if message.get("id").is_some() {
                return (line, message);
            }
            assert!(
                message["method"].is_string(),
                "invalid MCP notification: {line}"
            );
        }
    })
    .await
    .expect(what)
}
