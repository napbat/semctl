//! One request with its retries and its call deadline.
//!
//! [`Client::send`] is the only place that sends a request again. It does so
//! for three causes:
//!
//! - The server is restoring a projection and answers `409` with `Retry-After`.
//! - The persisted tenant is stale and one repair can fix it.
//! - A gateway or the connection failed, and the request is safe to repeat.
//!
//! A client that carries a deadline keeps every wait and every attempt inside
//! it. The deadline ends [`DEADLINE_MARGIN`] early, so the typed error reaches
//! the caller before the caller's own backstop fires.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use tokio::{sync::OwnedSemaphorePermit, time::Instant};
use tracing::debug;

use super::{
    ApiFailure, Client, LoadingKind, envelope_failure,
    failure::{response_body_error, retry_after},
    graph_state,
    loading_memo::{CodebaseRequest, LoadingMemo},
    tenant_binding_denied,
};

/// How long one call waits for a loading projection, inside any call deadline.
/// Session history showed graph restores that lasted minutes, and a wait until
/// the call deadline almost never got an answer. A short wait still absorbs a
/// restore that ends at once.
const LOADING_RETRY_BUDGET: Duration = Duration::from_secs(5);

/// The time kept before a call deadline. A request never uses it, so the
/// client can report a typed failure before the caller cuts the call off.
pub(crate) const DEADLINE_MARGIN: Duration = Duration::from_millis(500);

/// The wait before the one retry of a gateway or connection failure.
const GATEWAY_RETRY_DELAY: Duration = Duration::from_secs(1);

/// The text that names a call deadline as the cause of a failed request.
const DEADLINE_PASSED: &str = "the call deadline passed before the server answered";

/// Whether the server holds the same state after a request is sent twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Idempotency {
    /// The request only reads. A gateway or connection failure can be retried.
    Idempotent,
    /// The request can write. After a failure the caller cannot know whether
    /// the server applied it, so only the server's own `409` loading answer,
    /// which states that it did nothing, repeats it.
    NotIdempotent,
}

/// The time left before the deadline, minus [`DEADLINE_MARGIN`]. `None` when
/// no deadline applies. A deadline in the past leaves zero.
fn usable_until(deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    deadline.map(|deadline| {
        deadline
            .saturating_duration_since(now)
            .saturating_sub(DEADLINE_MARGIN)
    })
}

/// Whether a wait of `delay` still leaves time for an attempt after it.
fn allows_wait(usable: Option<Duration>, delay: Duration) -> bool {
    usable.is_none_or(|usable| delay < usable)
}

/// The statuses of a gateway that did not get an answer from the server.
fn is_gateway_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 502 | 503 | 504 | 530)
}

fn deadline_passed() -> anyhow::Error {
    anyhow!(DEADLINE_PASSED)
}

/// One call's loading policy and the last loading answer before a retry.
struct LoadingRetry<'a> {
    memo: &'a LoadingMemo,
    request: Option<CodebaseRequest<'a>>,
    started: Instant,
    previous: Option<anyhow::Error>,
}

impl<'a> LoadingRetry<'a> {
    fn new(memo: &'a LoadingMemo, path: &'a str) -> Self {
        Self {
            memo,
            request: CodebaseRequest::from_path(path),
            started: Instant::now(),
            previous: None,
        }
    }

    fn retry_after(&mut self, client: &Client, error: anyhow::Error) -> Result<Duration> {
        let Some(failure) = error.downcast_ref::<ApiFailure>() else {
            return Err(error);
        };
        let Some(kind) = failure.loading_kind() else {
            return Err(error);
        };
        if let Some(delay) = failure.loading_delay()
            && client.wait_for_loading
            && !self
                .request
                .as_ref()
                .is_some_and(|request| self.memo.contains(request.id, kind, self.started))
            && delay < LOADING_RETRY_BUDGET.saturating_sub(self.started.elapsed())
            && allows_wait(usable_until(client.deadline, Instant::now()), delay)
        {
            self.previous = self.request.as_ref().map(|_| error);
            return Ok(delay);
        }
        self.record(failure, kind);
        Err(error)
    }

    fn on_deadline(&mut self, error: anyhow::Error) -> anyhow::Error {
        let Some(previous) = self.previous.take() else {
            return error;
        };
        if let Some(failure) = previous.downcast_ref::<ApiFailure>()
            && let Some(kind) = failure.loading_kind()
        {
            self.record(failure, kind);
        }
        previous.context(format!("{error:#}"))
    }

    fn record(&self, failure: &ApiFailure, kind: LoadingKind) {
        if let Some(request) = &self.request {
            self.memo
                .record(request.id, kind, Instant::now(), failure.loading_window());
        }
    }

    fn clear(&self) {
        if let Some(request) = &self.request
            && let Some(kind) = request.kind
        {
            self.memo.clear(request.id, kind);
        }
    }
}

impl Client {
    /// Send one request, repairing a stale persisted tenant once, honoring the
    /// server's bounded `Retry-After` contract for transient graph/file
    /// projection restores, and repeating a safe request once after a gateway
    /// or connection failure.
    ///
    /// Every repeat stays inside the client's deadline, when it has one. A
    /// loading answer that no longer fits is returned as it is, so the caller
    /// reads its typed retry delay.
    pub(super) async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
        idempotency: Idempotency,
    ) -> Result<(Response, String)> {
        let mut loading = LoadingRetry::new(&self.loading_memo, path);
        let mut tenant_retried = false;
        let mut gateway_retried = false;
        loop {
            // Acquired after the token fetch: that request is authorization, not
            // an interactive read, and waiting for a permit while holding one
            // would make the bound self-blocking.
            let (req, url, rejected_tenant) = self
                .before_deadline(self.authed(method.clone(), path))
                .await
                .map_err(|error| loading.on_deadline(error))??;
            let req = match &body {
                Some(json) => req.json(json),
                None => req,
            };
            let (req, permit) = self
                .admit(req)
                .await
                .map_err(|error| loading.on_deadline(error))?;
            let resp = match req.send().await {
                Ok(resp) => resp,
                Err(error) => {
                    if error.is_connect()
                        && self.take_gateway_retry(idempotency, &mut gateway_retried)
                    {
                        debug!(%error, "connection failed; retrying once");
                        drop(permit);
                        tokio::time::sleep(GATEWAY_RETRY_DELAY).await;
                        continue;
                    }
                    let cut_off = self.cut_off_by_deadline(&error);
                    let error = transport_error(error, &method, &url, cut_off);
                    return Err(if cut_off {
                        loading.on_deadline(error)
                    } else {
                        error
                    });
                }
            };

            let status = resp.status();
            if status == StatusCode::CONFLICT {
                // Only retry an HTTP conflict when its body confirms loading.
                // Like the envelope readers, do not hold a permit for the body.
                drop(permit);
                let error = self
                    .read_conflict(resp, &method, &url, &mut loading)
                    .await?;
                let delay = loading.retry_after(self, error)?;
                debug!(
                    %status,
                    retry_after_ms = delay.as_millis(),
                    "server projection is restoring; retrying request"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            if is_gateway_status(status)
                && self.take_gateway_retry(idempotency, &mut gateway_retried)
            {
                debug!(%status, "gateway failed; retrying once");
                drop(permit);
                tokio::time::sleep(GATEWAY_RETRY_DELAY).await;
                continue;
            }

            if status.is_success() {
                loading.clear();
                // A partial graph answer is the best result the server has.
                // A retry cannot improve it at once, so the client keeps it.
                graph_state::observe(
                    method.as_str(),
                    &url,
                    resp.headers(),
                    self.partial_answers.as_ref(),
                );
            }
            if status != StatusCode::FORBIDDEN {
                return Ok((resp, url));
            }

            let response_body = resp
                .text()
                .await
                .with_context(|| format!("{method} {url}: read body"))?;
            // Tenant repair queries identity and rewrites config. That is not
            // this request attempt, so it must not hold this attempt's permit.
            drop(permit);
            if !tenant_retried
                && tenant_binding_denied(&response_body)
                && self
                    .repair_tenant_after_denial(rejected_tenant.as_deref())
                    .await
            {
                tenant_retried = true;
                continue;
            }
            return Err(response_body_error(
                method.as_str(),
                &url,
                status,
                &response_body,
            ));
        }
    }

    async fn read_conflict(
        &self,
        resp: Response,
        method: &Method,
        url: &str,
        loading: &mut LoadingRetry<'_>,
    ) -> Result<anyhow::Error> {
        let retry_after = retry_after(resp.headers());
        let status = resp.status();
        let body = resp.text().await.map_err(|error| {
            let cut_off = self.cut_off_by_deadline(&error);
            let error = anyhow::Error::new(error).context(format!("{method} {url}: read body"));
            if cut_off {
                loading.on_deadline(error)
            } else {
                error
            }
        })?;
        Ok(envelope_failure(
            method.as_str(),
            url,
            status,
            retry_after,
            &body,
        ))
    }

    /// Run one step that is not an HTTP attempt, such as a token refresh or the
    /// wait for a permit, and fail when it outlasts the deadline.
    async fn before_deadline<T>(&self, step: impl Future<Output = T>) -> Result<T> {
        let Some(deadline) = self.deadline else {
            return Ok(step.await);
        };
        let limit = deadline.checked_sub(DEADLINE_MARGIN).unwrap_or(deadline);
        tokio::time::timeout_at(limit, step)
            .await
            .map_err(|_| deadline_passed())
    }

    /// Take the permit for one attempt, then bound the attempt by what remains
    /// of the deadline. Without time left, nothing is sent.
    async fn admit(
        &self,
        req: RequestBuilder,
    ) -> Result<(RequestBuilder, Option<OwnedSemaphorePermit>)> {
        let permit = self.before_deadline(self.remote_permit()).await?;
        match usable_until(self.deadline, Instant::now()) {
            None => Ok((req, permit)),
            Some(usable) if usable.is_zero() => Err(deadline_passed()),
            // The request timeout covers connecting, sending, and reading the
            // body, so a slow body cannot outlast the deadline either.
            Some(usable) => Ok((req.timeout(usable), permit)),
        }
    }

    /// Claim the one gateway retry of a request, when the request is safe to
    /// repeat and the deadline leaves time for it.
    fn take_gateway_retry(&self, idempotency: Idempotency, used: &mut bool) -> bool {
        let allowed = idempotency == Idempotency::Idempotent
            && !*used
            && allows_wait(
                usable_until(self.deadline, Instant::now()),
                GATEWAY_RETRY_DELAY,
            );
        *used |= allowed;
        allowed
    }

    fn cut_off_by_deadline(&self, error: &reqwest::Error) -> bool {
        self.deadline.is_some() && error.is_timeout() && !error.is_connect()
    }
}

/// An attempt cut off by the call deadline says so. The transport's own
/// connect timeout is separate.
fn transport_error(
    error: reqwest::Error,
    method: &Method,
    url: &str,
    cut_off: bool,
) -> anyhow::Error {
    let error = anyhow::Error::new(error);
    let error = if cut_off {
        error.context(DEADLINE_PASSED)
    } else {
        error
    };
    error.context(format!("{method} {url}"))
}

#[cfg(test)]
mod tests;
