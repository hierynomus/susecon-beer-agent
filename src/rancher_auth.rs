use std::time::Duration;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Auth error
// ---------------------------------------------------------------------------

pub(crate) enum AuthError {
    MissingHeaders,
    RancherUnreachable(String),
    InvalidToken(String),
    BadGateway(String),
    Forbidden { username: String, required_role: String, actual_roles: Vec<String> },
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        match self {
            Self::MissingHeaders => {
                warn!("Auth failed: missing R_token and/or R_url headers");
                (StatusCode::UNAUTHORIZED, "Unauthorized: R_token and R_url headers are required")
                    .into_response()
            }
            Self::RancherUnreachable(detail) => {
                error!(detail, "Auth failed: could not reach Rancher");
                (StatusCode::BAD_GATEWAY, "Authentication failed: unable to reach Rancher server")
                    .into_response()
            }
            Self::InvalidToken(detail) => {
                warn!(detail, "Auth failed: invalid or expired token");
                (StatusCode::UNAUTHORIZED, "Unauthorized: invalid or expired Rancher token")
                    .into_response()
            }
            Self::BadGateway(detail) => {
                error!(detail, "Auth failed: unexpected Rancher response");
                (StatusCode::BAD_GATEWAY, "Authentication failed: unexpected response from Rancher")
                    .into_response()
            }
            Self::Forbidden { username, required_role, actual_roles } => {
                warn!(
                    %username,
                    %required_role,
                    ?actual_roles,
                    "Auth DENIED: user does not have the required role"
                );
                (
                    StatusCode::FORBIDDEN,
                    format!("Forbidden: user \"{username}\" does not have the required role \"{required_role}\""),
                )
                    .into_response()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rancher API types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RancherUser {
    id: String,
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RancherCollection<T> {
    data: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GlobalRoleBinding {
    global_role_id: String,
}

// ---------------------------------------------------------------------------
// Auth state + Rancher client logic
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct RancherAuthState {
    http_client: reqwest::Client,
    required_role: String,
}

impl RancherAuthState {
    pub fn new(required_role: String, tls_verify: bool) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .danger_accept_invalid_certs(!tls_verify)
            .build()
            .expect("failed to build reqwest client");

        Self {
            http_client,
            required_role,
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: &str,
    ) -> Result<T, AuthError> {
        let resp = self
            .http_client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AuthError::RancherUnreachable(format!("{url}: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AuthError::InvalidToken(format!(
                "Rancher returned {status} for {url}. Body: {body}"
            )));
        }

        resp.json()
            .await
            .map_err(|e| AuthError::BadGateway(format!("failed to parse {url}: {e}")))
    }

    async fn authenticate(&self, token: &str, rancher_url: &str) -> Result<String, AuthError> {
        // Identify the user
        let me_url = format!("{rancher_url}/v3/users?me");
        let users: RancherCollection<RancherUser> = self.get_json(&me_url, token).await?;

        let user = users
            .data
            .into_iter()
            .next()
            .ok_or_else(|| AuthError::InvalidToken("empty user list".into()))?;

        let username = user.username.unwrap_or_else(|| "unknown".into());
        info!(%username, user_id = %user.id, "Authenticated Rancher user");

        // Check global role bindings
        let grb_url = format!("{rancher_url}/v3/globalRoleBindings?userId={}", user.id);
        let bindings: RancherCollection<GlobalRoleBinding> =
            self.get_json(&grb_url, token).await?;

        let role_ids: Vec<String> = bindings
            .data
            .into_iter()
            .map(|b| b.global_role_id)
            .collect();

        if role_ids.iter().any(|r| r == &self.required_role) {
            info!(%username, role = %self.required_role, "User has the required role");
            Ok(username)
        } else {
            Err(AuthError::Forbidden {
                username,
                required_role: self.required_role.clone(),
                actual_roles: role_ids,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Axum middleware
// ---------------------------------------------------------------------------

pub async fn rancher_auth_middleware(
    State(state): State<RancherAuthState>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, AuthError> {
    let headers = req.headers();
    let r_token = headers
        .get("R_token")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty());
    let r_url = headers
        .get("R_url")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty());

    // If neither auth header is present, allow through (e.g. MCP discovery).
    // If only one is present, that's a misconfiguration — reject.
    match (r_token, r_url) {
        (None, None) => {
            info!("Auth middleware: no auth headers, allowing through (discovery)");
            return Ok(next.run(req).await);
        }
        (Some(_), None) | (None, Some(_)) => {
            warn!("Auth middleware: only one of R_token/R_url present — rejecting");
            return Err(AuthError::MissingHeaders);
        }
        (Some(token), Some(url)) => {
            let url = url.trim_end_matches('/');
            info!(r_url = url, "Auth middleware: validating Rancher credentials");
            state.authenticate(token, url).await?;
        }
    }

    Ok(next.run(req).await)
}
