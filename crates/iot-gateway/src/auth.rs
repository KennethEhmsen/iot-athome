//! Bearer-token validation against an OIDC issuer.
//!
//! Config-gated: when [`OidcConfig`] is absent, the middleware is a no-op
//! (dev-mode "trust the upstream"). When present, every request routed
//! through [`bearer_middleware`] must carry a valid RS256-signed JWT whose
//! `iss` matches `issuer_url` and whose `aud` contains `audience`.
//!
//! JWKS fetch: one HTTP GET at startup (and on cache expiry). Keys are
//! indexed by `kid`. Failed lookups force a refresh before rejecting.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{debug, warn};

#[derive(Debug, Clone, Deserialize)]
pub struct OidcConfig {
    /// Issuer URL (no trailing slash), e.g.
    /// `http://localhost:8080/realms/iotathome`.
    pub issuer_url: String,
    /// Expected `aud` claim (OIDC client id of a protected client), e.g.
    /// `iot-gateway`.
    pub audience: String,
    /// JWKS cache lifetime. 300s is a sensible default.
    #[serde(default = "default_jwks_ttl")]
    pub jwks_cache_secs: u64,
}

const fn default_jwks_ttl() -> u64 {
    300
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("missing bearer token")]
    MissingToken,
    #[error("malformed bearer token")]
    MalformedToken,
    #[error("no kid in JWT header")]
    MissingKid,
    #[error("unknown kid: {0}")]
    UnknownKid(String),
    #[error("jwks fetch: {0}")]
    Jwks(String),
    #[error("jwt validation: {0}")]
    Validation(String),
}

impl AuthError {
    const fn status(&self) -> StatusCode {
        match self {
            Self::Jwks(_) => StatusCode::BAD_GATEWAY,
            _ => StatusCode::UNAUTHORIZED,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Verifier {
    cfg: OidcConfig,
    jwks: Arc<RwLock<JwksCache>>,
    http: reqwest::Client,
}

struct JwksCache {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Option<Instant>,
}

impl std::fmt::Debug for JwksCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwksCache")
            .field("keys", &self.keys.keys().collect::<Vec<_>>())
            .field("fetched_at", &self.fetched_at)
            .finish()
    }
}

impl Verifier {
    #[must_use]
    pub fn new(cfg: OidcConfig) -> Self {
        Self {
            cfg,
            jwks: Arc::new(RwLock::new(JwksCache {
                keys: HashMap::new(),
                fetched_at: None,
            })),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    async fn key_for(&self, kid: &str) -> Result<DecodingKey, AuthError> {
        {
            let cache = self.jwks.read().await;
            if let Some(k) = cache.keys.get(kid) {
                if cache
                    .fetched_at
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(self.cfg.jwks_cache_secs))
                {
                    return Ok(k.clone());
                }
            }
        }
        self.refresh().await?;
        let cache = self.jwks.read().await;
        cache
            .keys
            .get(kid)
            .cloned()
            .ok_or_else(|| AuthError::UnknownKid(kid.to_owned()))
    }

    async fn refresh(&self) -> Result<(), AuthError> {
        let jwks_url = format!("{}/protocol/openid-connect/certs", self.cfg.issuer_url);
        let body: JwksDoc = self
            .http
            .get(&jwks_url)
            .send()
            .await
            .map_err(|e| AuthError::Jwks(e.to_string()))?
            .error_for_status()
            .map_err(|e| AuthError::Jwks(e.to_string()))?
            .json()
            .await
            .map_err(|e| AuthError::Jwks(e.to_string()))?;

        let mut fresh = HashMap::new();
        for jwk in body.keys {
            if jwk.kty != "RSA" || jwk.alg.as_deref().unwrap_or("RS256") != "RS256" {
                continue;
            }
            match DecodingKey::from_rsa_components(&jwk.n, &jwk.e) {
                Ok(k) => {
                    fresh.insert(jwk.kid, k);
                }
                Err(e) => warn!(kid = %jwk.kid, error = %e, "bad jwk"),
            }
        }
        debug!(count = fresh.len(), "refreshed jwks");
        let mut cache = self.jwks.write().await;
        cache.keys = fresh;
        cache.fetched_at = Some(Instant::now());
        Ok(())
    }

    /// Validate a raw JWT string. Returns the parsed claims on success.
    pub async fn verify(&self, token: &str) -> Result<Claims, AuthError> {
        let header = decode_header(token).map_err(|e| AuthError::Validation(e.to_string()))?;
        let kid = header.kid.ok_or(AuthError::MissingKid)?;
        let key = self.key_for(&kid).await?;

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(std::slice::from_ref(&self.cfg.audience));
        validation.set_issuer(std::slice::from_ref(&self.cfg.issuer_url));
        validation.leeway = 30;

        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| AuthError::Validation(e.to_string()))?;
        Ok(data.claims)
    }
}

#[derive(Debug, Deserialize)]
struct JwksDoc {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    alg: Option<String>,
    kid: String,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: i64,
    #[serde(default)]
    pub iat: Option<i64>,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

/// Axum middleware: validate Bearer token on every request.
pub async fn bearer_middleware(
    State(state): State<crate::state::AppState>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(verifier) = state.verifier.as_ref() else {
        return Ok(next.run(req).await);
    };
    let token = extract_bearer(req.headers()).ok_or(StatusCode::UNAUTHORIZED)?;
    verifier.verify(&token).await.map_err(|e| {
        warn!(error = %e, "bearer rejected");
        e.status()
    })?;
    Ok(next.run(req).await)
}

/// Extract the raw token from an `Authorization: Bearer <...>` header.
pub fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(ToOwned::to_owned)
}
