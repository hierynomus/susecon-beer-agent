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
#[serde(rename_all = "camelCase")]
struct RancherPrincipal {
    id: String,
    login_name: Option<String>,
    display_name: Option<String>,
    principal_type: Option<String>,
    me: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RancherCollection<T> {
    data: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GlobalRoleBinding {
    global_role_id: String,
    user_id: Option<String>,
    group_principal_id: Option<String>,
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

        let body = resp
            .text()
            .await
            .map_err(|e| AuthError::BadGateway(format!("failed to read body from {url}: {e}")))?;

        tracing::debug!(url, body, "Rancher API response");

        serde_json::from_str(&body)
            .map_err(|e| AuthError::BadGateway(format!("failed to parse {url}: {e}. Body: {body}")))
    }

    async fn authenticate(&self, token: &str, rancher_url: &str) -> Result<String, AuthError> {
        // Identify the current principal
        let principals_url = format!("{rancher_url}/v3/principals");
        let principals: RancherCollection<RancherPrincipal> =
            self.get_json(&principals_url, token).await?;

        let me = principals
            .data
            .iter()
            .find(|p| p.me == Some(true))
            .or_else(|| principals.data.first())
            .ok_or_else(|| AuthError::InvalidToken("no principals returned".into()))?;

        let display_name = me
            .display_name
            .as_deref()
            .or(me.login_name.as_deref())
            .unwrap_or("unknown");

        info!(
            display_name,
            principal_id = %me.id,
            principal_type = me.principal_type.as_deref().unwrap_or("unknown"),
            "Authenticated Rancher principal"
        );

        // Collect all principal IDs for this user (user + group principals)
        let my_principal_ids: Vec<&str> = principals
            .data
            .iter()
            .map(|p| p.id.as_str())
            .collect();

        info!(?my_principal_ids, "User's principal IDs");

        // Fetch all global role bindings and match against our principal IDs
        // We need to check both userId-based and groupPrincipalId-based bindings
        let grb_url = format!("{rancher_url}/v3/globalRoleBindings");
        let bindings: RancherCollection<GlobalRoleBinding> =
            self.get_json(&grb_url, token).await?;

        let matching_roles: Vec<&str> = bindings
            .data
            .iter()
            .filter(|b| {
                // Match by userId (principal ID prefix before ://)
                let user_match = b.user_id.as_deref().map_or(false, |uid| {
                    my_principal_ids.iter().any(|pid| pid.ends_with(uid))
                });
                // Match by groupPrincipalId
                let group_match = b.group_principal_id.as_deref().map_or(false, |gid| {
                    my_principal_ids.contains(&gid)
                });
                user_match || group_match
            })
            .map(|b| b.global_role_id.as_str())
            .collect();

        info!(?matching_roles, "User's matching global roles");

        if matching_roles.iter().any(|r| *r == self.required_role) {
            info!(display_name, role = %self.required_role, "User has the required role");
            Ok(display_name.to_string())
        } else {
            Err(AuthError::Forbidden {
                username: display_name.to_string(),
                required_role: self.required_role.clone(),
                actual_roles: matching_roles.into_iter().map(String::from).collect(),
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
            info!(r_url = url, r_token = token, "Auth middleware: validating Rancher credentials");
            state.authenticate(token, url).await?;
        }
    }

    Ok(next.run(req).await)
}
