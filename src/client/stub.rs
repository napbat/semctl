//! A loopback HTTP server that answers from a script, for tests of the client.
//!
//! The server listens on the loopback interface only, so a test needs no
//! network. Each answer closes its connection, so every request is one new
//! connection and the request count is exact.

use std::fmt::Write as _;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One scripted reaction to one request.
#[derive(Clone, Debug)]
pub(crate) enum Reply {
    /// Answer with this status line, headers, and body.
    Answer {
        status: &'static str,
        headers: Vec<(&'static str, &'static str)>,
        body: String,
    },
    /// Read the request and never answer.
    Silent,
}

impl Reply {
    /// `200 OK` with the server's JSON envelope around `data`.
    pub(crate) fn ok(data: &str) -> Self {
        Self::Answer {
            status: "200 OK",
            headers: Vec::new(),
            body: format!(r#"{{"success":true,"data":{data}}}"#),
        }
    }

    /// The `409` answer of a server that is restoring its graph.
    pub(crate) fn loading(retry_after: &'static str) -> Self {
        Self::Answer {
            status: "409 Conflict",
            headers: vec![("Retry-After", retry_after)],
            body: r#"{"success":false,"errors":[{"code":"GraphLoading","message":"loading"}]}"#
                .to_string(),
        }
    }

    /// A gateway's answer: an HTML page, not the server's envelope.
    pub(crate) fn gateway(status: &'static str) -> Self {
        Self::Answer {
            status,
            headers: vec![("Content-Type", "text/html")],
            body: "<html>gateway failure</html>".to_string(),
        }
    }
}

/// A running stub. Dropping it does not stop the listener; the test runtime
/// ends it.
pub(crate) struct Stub {
    /// The base URL of the server, without a trailing slash.
    pub(crate) url: String,
    requests: Arc<AtomicUsize>,
    heads: Arc<Mutex<Vec<String>>>,
}

impl Stub {
    /// Serve `script` on a new loopback port: the first reply answers the first
    /// request, and so on. The last reply answers every later request.
    pub(crate) async fn serve(script: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        Self::serve_on(listener, script)
    }

    /// Like [`Self::serve`], on a listener the test already holds.
    pub(crate) fn serve_on(listener: TcpListener, script: Vec<Reply>) -> Self {
        assert!(!script.is_empty(), "a stub needs at least one reply");
        let url = format!("http://{}", listener.local_addr().expect("local address"));
        let requests = Arc::new(AtomicUsize::new(0));
        let heads = Arc::new(Mutex::new(Vec::new()));
        tokio::spawn({
            let requests = Arc::clone(&requests);
            let heads = Arc::clone(&heads);
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let index = requests.fetch_add(1, Ordering::SeqCst);
                    let reply = script[index.min(script.len() - 1)].clone();
                    tokio::spawn(answer(stream, reply, Arc::clone(&heads)));
                }
            }
        });
        Self {
            url,
            requests,
            heads,
        }
    }

    /// How many requests have arrived.
    pub(crate) fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// The first line of every request that has arrived, such as
    /// `GET /v1/probe HTTP/1.1`.
    pub(crate) fn request_lines(&self) -> Vec<String> {
        self.heads.lock().expect("the stub log").clone()
    }
}

async fn answer(mut stream: TcpStream, reply: Reply, heads: Arc<Mutex<Vec<String>>>) {
    let mut request = Vec::new();
    let Some(head_end) = read_request(&mut stream, &mut request).await else {
        return;
    };
    let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
    let line = head.lines().next().unwrap_or_default().to_string();
    heads.lock().expect("the stub log").push(line);

    let Reply::Answer {
        status,
        headers,
        body,
    } = reply
    else {
        // Hold the connection open until the client gives up.
        let _ = stream.read(&mut [0_u8; 16]).await;
        return;
    };
    let mut raw = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        write!(raw, "{name}: {value}\r\n").expect("write to a string");
    }
    raw.push_str("\r\n");
    raw.push_str(&body);
    // The client may have closed already, which is a normal end for a test.
    let _ = stream.write_all(raw.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Read one request, head and body, into `request`. Returns the length of the
/// head, or `None` when the client closed first.
async fn read_request(stream: &mut TcpStream, request: &mut Vec<u8>) -> Option<usize> {
    let mut buffer = [0_u8; 1024];
    let head_end = loop {
        if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return None,
            Ok(count) => request.extend_from_slice(&buffer[..count]),
        }
    };
    let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while request.len() < head_end + length {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return None,
            Ok(count) => request.extend_from_slice(&buffer[..count]),
        }
    }
    Some(head_end)
}
