//! The HTTP surface: `POST /rpc` for the protocol and `POST /lifecycle` for
//! local administration.
//!
//! Security posture is inherited deliberately from the TypeScript host:
//! the server binds to loopback only, rejects any request carrying an `Origin`
//! header, accepts `POST` to exactly two paths, and requires a paired device
//! credential. There is no CORS escape hatch.

use crate::{
    mutex::Mutex,
    protocol::{RpcFailure, RpcRequest, HOST_PROTOCOL_VERSION, MAX_BODY},
    store::HostStore,
};
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::broadcast;

/// Everything a request needs. The store is synchronous and serialised behind
/// a mutex: the host is the only writer, and a headless server's throughput is
/// dominated by provider I/O, not database calls.
pub struct HostState {
    pub store: Arc<Mutex<HostStore>>,
    /// Local administrative credential for `/lifecycle`. Deliberately distinct
    /// from any paired device token.
    pub admin_secret: String,
    pub providers: Vec<String>,
    pub shutdown: broadcast::Sender<()>,
}

pub fn router(state: Arc<HostState>) -> Router {
    Router::new()
        .route("/rpc", post(rpc))
        .route("/lifecycle", post(lifecycle))
        .fallback(unsupported)
        .layer(tower_http::limit::RequestBodyLimitLayer::new(MAX_BODY))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(20),
        ))
        .with_state(state)
}

/// Any route other than the two above is refused, not silently accepted.
async fn unsupported() -> Response {
    failure(RpcFailure::Forbidden(
        "Unsupported request origin or route".into(),
    ))
}

fn failure(error: RpcFailure) -> Response {
    let status = match &error {
        RpcFailure::Forbidden(_) => StatusCode::FORBIDDEN,
        RpcFailure::Unauthorized(_) => StatusCode::UNAUTHORIZED,
        RpcFailure::BadRequest(_) => StatusCode::BAD_REQUEST,
    };
    let body = serde_json::to_vec(&crate::protocol::RpcError {
        error: error.message().to_string(),
    })
    .unwrap_or_else(|_| b"{\"error\":\"Host request failed\"}".to_vec());
    json_response(status, body)
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}

/// A browser sends `Origin`; the desktop client does not. Rejecting any request
/// that carries one closes the drive-by cross-origin path entirely.
fn reject_origin(headers: &HeaderMap) -> Result<(), RpcFailure> {
    if headers.contains_key(header::ORIGIN) {
        return Err(RpcFailure::Forbidden(
            "Unsupported request origin or route".into(),
        ));
    }
    Ok(())
}

async fn rpc(State(state): State<Arc<HostState>>, request: Request<Body>) -> Response {
    match rpc_inner(state, request).await {
        Ok(body) => {
            let status = StatusCode::OK;
            json_response(status, body)
        }
        Err(error) => failure(error),
    }
}

async fn rpc_inner(state: Arc<HostState>, request: Request<Body>) -> Result<Vec<u8>, RpcFailure> {
    let headers = request.headers().clone();
    reject_origin(&headers)?;

    let token = RpcFailure::bearer(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
    )
    .ok_or_else(|| RpcFailure::Unauthorized("Device credential is invalid or revoked".into()))?;

    {
        let store = state.store.lock().await;
        if !store
            .authenticated(&token)
            .map_err(RpcFailure::BadRequest)?
        {
            return Err(RpcFailure::Unauthorized(
                "Device credential is invalid or revoked".into(),
            ));
        }
    }

    let body = axum::body::to_bytes(request.into_body(), MAX_BODY)
        .await
        .map_err(|_| RpcFailure::BadRequest("Request is too large".into()))?;
    let value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| RpcFailure::BadRequest("Invalid request".into()))?;
    if !value.is_object() {
        return Err(RpcFailure::BadRequest("Invalid request".into()));
    }
    let request: RpcRequest = serde_json::from_value(value)
        .map_err(|_| RpcFailure::BadRequest("Invalid request".into()))?;

    // Reading the body yields: a device may have been revoked since the
    // headers arrived, so authenticate again before dispatching anything.
    {
        let store = state.store.lock().await;
        if !store
            .authenticated(&token)
            .map_err(RpcFailure::BadRequest)?
        {
            return Err(RpcFailure::Unauthorized(
                "Device credential is invalid or revoked".into(),
            ));
        }
    }

    if request.version != HOST_PROTOCOL_VERSION {
        return Err(RpcFailure::BadRequest(
            "Incompatible protocol version".into(),
        ));
    }

    let dispatching = crate::methods::Dispatch {
        state: Arc::clone(&state),
        token,
    };
    let result = dispatching
        .dispatch(request.method, request.environment_id, request.params)
        .await?;
    serde_json::to_vec(&crate::protocol::RpcSuccess { result })
        .map_err(|e| RpcFailure::BadRequest(e.to_string()))
}

async fn lifecycle(
    State(state): State<Arc<HostState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if reject_origin(&headers).is_err() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {}", state.admin_secret));
    if !authorized {
        return StatusCode::FORBIDDEN.into_response();
    }
    let bytes = match axum::body::to_bytes(body, 128).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let request: crate::protocol::LifecycleRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match request.action.as_str() {
        "status" => json_response(StatusCode::OK, b"{}".to_vec()),
        "stop" => {
            // Answer before signalling, so the caller always sees a reply to a
            // stop it caused.
            let response = json_response(StatusCode::OK, b"{}".to_vec());
            let shutdown = state.shutdown.clone();
            tokio::spawn(async move {
                let _ = shutdown.send(());
            });
            response
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt as _;

    async fn state() -> (tempfile::TempDir, Arc<HostState>, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = HostStore::open(dir.path().join("host.db")).unwrap();
        let device = store.issue_device("Laptop").unwrap();
        let (shutdown, _) = broadcast::channel(4);
        let state = Arc::new(HostState {
            store: Arc::new(Mutex::new(store)),
            admin_secret: "admin".into(),
            providers: vec![],
            shutdown,
        });
        (dir, state, device.token)
    }

    fn post(path: &str, token: Option<&str>) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder().method("POST").uri(path);
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"version":1,"method":"environment.describe"}"#,
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn a_valid_device_reaches_the_protocol() {
        let (_dir, state, token) = state().await;
        let response = router(state)
            .oneshot(post("/rpc", Some(&token)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["result"]["protocolVersion"], 1);
    }

    #[tokio::test]
    async fn a_request_without_a_credential_is_refused() {
        let (_dir, state, _token) = state().await;
        let response = router(state).oneshot(post("/rpc", None)).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn an_unknown_credential_is_refused() {
        let (_dir, state, _token) = state().await;
        let response = router(state)
            .oneshot(post("/rpc", Some(&"a".repeat(43))))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_browser_origin_is_refused_even_with_a_valid_credential() {
        let (_dir, state, token) = state().await;
        let mut request = post("/rpc", Some(&token));
        request
            .headers_mut()
            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_unrelated_route_is_refused() {
        let (_dir, state, token) = state().await;
        let request = HttpRequest::builder()
            .uri("/admin")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_wrong_protocol_version_is_refused() {
        let (_dir, state, token) = state().await;
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/rpc")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(
                r#"{"version":99,"method":"environment.describe"}"#,
            ))
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn stop_requires_the_administrative_credential() {
        let (_dir, state, token) = state().await;
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/lifecycle")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(r#"{"action":"stop"}"#))
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a paired device token must not be able to stop the host"
        );
    }

    #[tokio::test]
    async fn status_answers_with_a_local_admin_credential() {
        let (_dir, state, _token) = state().await;
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/lifecycle")
            .header(header::AUTHORIZATION, "Bearer admin")
            .body(Body::from(r#"{"action":"status"}"#))
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
