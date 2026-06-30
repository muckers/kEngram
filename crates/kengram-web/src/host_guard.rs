//! Host-header guard for the web routes.
//!
//! rmcp enforces its `allowed_hosts` DNS-rebinding check *inside* the `/mcp`
//! service, so the `/api/*` and page routes get no protection for free. This
//! middleware replicates the same policy against `[server].allowed_hosts` so the
//! web surface's exposure is identical to the MCP surface's — the operator
//! manages one list.
//!
//! Policy: an empty list means "loopback only" (rmcp's safe default —
//! `localhost`/`127.0.0.1`/`::1`); a non-empty list is the operator allowlist
//! and is matched against both the full `Host` (with port) and the bare host,
//! mirroring the rmcp matcher (see the `[server].allowed_hosts` doc in
//! `kengram-cli/src/config.rs`).

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::HOST;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::WebState;

/// Extract the hostname from a `Host` header value, dropping any port and
/// IPv6 brackets (`[::1]:8081` -> `::1`, `repromax:8081` -> `repromax`).
fn host_only(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        // `[ipv6]` or `[ipv6]:port`
        return rest.split(']').next().unwrap_or(rest);
    }
    host.split(':').next().unwrap_or(host)
}

/// Whether a `Host` header is allowed under the given allowlist.
pub(crate) fn host_allowed(host: &str, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        // rmcp's safe default: loopback only.
        matches!(host_only(host), "localhost" | "127.0.0.1" | "::1")
    } else {
        // Match the full host (with port) or the bare host, mirroring rmcp.
        allowed.iter().any(|a| a == host) || allowed.iter().any(|a| a == host_only(host))
    }
}

/// axum middleware: reject requests whose `Host` header isn't allowed.
pub async fn guard(State(state): State<WebState>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if host_allowed(host, &state.allowed_hosts) {
        next.run(req).await
    } else {
        tracing::warn!(%host, "web: rejected request with disallowed Host header");
        (StatusCode::FORBIDDEN, "disallowed Host header").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_allowlist_accepts_loopback_only() {
        let allowed: Vec<String> = vec![];
        assert!(host_allowed("localhost", &allowed));
        assert!(host_allowed("localhost:8081", &allowed));
        assert!(host_allowed("127.0.0.1", &allowed));
        assert!(host_allowed("127.0.0.1:8081", &allowed));
        assert!(host_allowed("[::1]:8081", &allowed));
        assert!(!host_allowed("repromax", &allowed));
        assert!(!host_allowed("beast.taila9ccc8.ts.net:8081", &allowed));
    }

    #[test]
    fn nonempty_allowlist_matches_full_and_bare_host() {
        let allowed = vec![
            "localhost".to_string(),
            "repromax".to_string(),
            "repromax:8081".to_string(),
        ];
        assert!(host_allowed("repromax", &allowed)); // bare in list
        assert!(host_allowed("repromax:8081", &allowed)); // full in list
        assert!(host_allowed("localhost:9999", &allowed)); // bare "localhost" matches
        assert!(!host_allowed("evil.example.com", &allowed));
        assert!(!host_allowed("evil.example.com:8081", &allowed));
    }
}
