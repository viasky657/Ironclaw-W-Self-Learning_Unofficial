use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

/// Bearer token authentication middleware.
///
/// Reads `IRONCLAW_HDC_SERVER_TOKEN` from the environment.
/// Rejects requests to protected endpoints without a valid bearer token.
/// Uses `subtle::ConstantTimeEq` for constant-time comparison to prevent
/// timing attacks.
///
/// Protected endpoints: `POST /v1/chat/completions`, `POST /v1/train`
/// Public endpoints: `GET /v1/models`, `GET /health`
pub async fn bearer_auth_middleware(
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = request.uri().path().to_string();
    let method = request.method().clone();

    // Public endpoints — no auth required.
    if is_public_endpoint(&method, &path) {
        return Ok(next.run(request).await);
    }

    // Protected endpoint — require bearer token.
    let expected_token = std::env::var("IRONCLAW_HDC_SERVER_TOKEN")
        .unwrap_or_default();

    if expected_token.is_empty() {
        tracing::warn!(
            path = %path,
            "HDC server: IRONCLAW_HDC_SERVER_TOKEN is not set — rejecting all write requests"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    let provided_token = extract_bearer_token(&headers);

    match provided_token {
        None => {
            tracing::warn!(path = %path, "HDC server: missing Authorization header");
            Err(StatusCode::UNAUTHORIZED)
        }
        Some(token) => {
            // Constant-time comparison to prevent timing attacks.
            let expected_bytes = expected_token.as_bytes();
            let provided_bytes = token.as_bytes();

            // subtle::ConstantTimeEq requires equal-length slices.
            // If lengths differ, the comparison is still constant-time (returns 0).
            let lengths_equal = expected_bytes.len() == provided_bytes.len();
            let tokens_equal = if lengths_equal {
                expected_bytes.ct_eq(provided_bytes).into()
            } else {
                // Perform a dummy comparison to maintain constant time.
                let _ = expected_bytes.ct_eq(expected_bytes);
                false
            };

            if tokens_equal {
                Ok(next.run(request).await)
            } else {
                tracing::warn!(path = %path, "HDC server: invalid bearer token");
                Err(StatusCode::UNAUTHORIZED)
            }
        }
    }
}

/// Return `true` for endpoints that do not require authentication.
fn is_public_endpoint(method: &axum::http::Method, path: &str) -> bool {
    matches!(
        (method, path),
        (&axum::http::Method::GET, "/v1/models")
            | (&axum::http::Method::GET, "/health")
    )
}

/// Extract the bearer token from the `Authorization` header.
fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    let auth = headers.get("Authorization")?.to_str().ok()?;
    auth.strip_prefix("Bearer ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Method;

    #[test]
    fn public_endpoints_are_not_protected() {
        assert!(is_public_endpoint(&Method::GET, "/v1/models"));
        assert!(is_public_endpoint(&Method::GET, "/health"));
    }

    #[test]
    fn write_endpoints_are_protected() {
        assert!(!is_public_endpoint(&Method::POST, "/v1/chat/completions"));
        assert!(!is_public_endpoint(&Method::POST, "/v1/train"));
    }

    #[test]
    fn extract_bearer_token_works() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            "Bearer my-secret-token".parse().unwrap(),
        );
        assert_eq!(extract_bearer_token(&headers), Some("my-secret-token"));
    }

    #[test]
    fn extract_bearer_token_missing() {
        let headers = HeaderMap::new();
        assert_eq!(extract_bearer_token(&headers), None);
    }

    #[test]
    fn extract_bearer_token_wrong_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Basic dXNlcjpwYXNz".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers), None);
    }
}
