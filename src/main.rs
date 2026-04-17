use std::time::Duration;

use anyhow::Result;
use axum::{Router, middleware, routing::get};
use rand::prelude::IndexedRandom;
use tracing_subscriber::prelude::*;
use rmcp::{
    ErrorData as McpError,
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    handler::server::common::Extension,
    model::*,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
        session::local::LocalSessionManager,
    },
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use tracing::info;

mod rancher_auth;
use rancher_auth::{AuthContext, RancherAuthState, rancher_auth_middleware};

// ---------------------------------------------------------------------------
// Response messages — one is chosen at random per order
// ---------------------------------------------------------------------------

const RESPONSES: &[&str] = &[
    "Order confirmed! Your {beer} is being pulled from the tap right now. \
     A human is on their way to you. Cheers! 🍺",
    "Beer ordered successfully! Our finest human courier is already heading \
     your way with your {beer}. Shouldn't be long!",
    "Done! The order went through. A real, live human is currently en route \
     with your {beer}. ETA: very soon. 🍻",
    "Your {beer} has been ordered. We've activated the human delivery protocol \
     — our most reliable operative is en route.",
    "Order received and confirmed! A friendly human has been dispatched with \
     your {beer}. They are extremely motivated.",
    "🍺 Tap room notified! Your {beer} is being poured and a human delivery \
     agent is heading your way as we speak.",
    "Confirmed! The {beer} order went through. A human — fully biological, \
     highly motivated — is on their way right now.",
    "Order placed! Your {beer} awaits. A human (the best delivery system ever \
     invented) is currently en route with your order.",
];

// ---------------------------------------------------------------------------
// Tool parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OrderBeerParams {
    /// The type of beer to order (e.g. "lager", "IPA", "stout").
    /// If not specified, any beer will do.
    pub beer_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_beer_name(beer_type: Option<String>) -> String {
    beer_type
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "beer".to_string())
}

// ---------------------------------------------------------------------------
// MCP service
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct BeerOrderService {
    required_role: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<BeerOrderService>,
}

#[tool_router]
impl BeerOrderService {
    pub fn new(required_role: String) -> Self {
        let mut svc = Self::default();
        svc.required_role = required_role;
        svc
    }

    /// Order a beer from the bar. A human will deliver it to you shortly.
    #[tool(
        description = "Order a beer from the bar. A human operative will be dispatched \
                       to deliver it to you shortly after the order is placed."
    )]
    async fn order_beer(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(params): Parameters<OrderBeerParams>,
    ) -> Result<CallToolResult, McpError> {
        // Check authorization: require the configured global role
        let auth = parts.extensions.get::<AuthContext>();
        match auth {
            None => {
                tracing::warn!("Beer order rejected: no auth context (unauthenticated request)");
                return Err(McpError::invalid_request(
                    "Authentication required to order beer. Please provide R_token and R_url headers.",
                    None,
                ));
            }
            Some(ctx) if !ctx.roles.iter().any(|r| r == &self.required_role) => {
                tracing::warn!(
                    user = %ctx.display_name,
                    required = %self.required_role,
                    actual = ?ctx.roles,
                    "Beer order rejected: missing required role"
                );
                return Err(McpError::invalid_request(
                    format!(
                        "Forbidden: user \"{}\" does not have the required role \"{}\"",
                        ctx.display_name, self.required_role
                    ),
                    None,
                ));
            }
            Some(ctx) => {
                tracing::info!(
                    user = %ctx.display_name,
                    role = %self.required_role,
                    "Beer order authorized"
                );
            }
        }

        let beer = resolve_beer_name(params.beer_type);

        info!("🍺  Incoming beer order: \"{}\"", beer);
        info!("⏳  Contacting tap room...");

        // First half of the fake processing delay
        let total_ms: u64 = rand::random_range(2_000u64..=4_000);
        tokio::time::sleep(Duration::from_millis(total_ms / 2)).await;
        info!("🔄  Order received by bar, preparing...");
        tokio::time::sleep(Duration::from_millis(total_ms / 2)).await;

        info!("✅  Order confirmed, dispatching human...");

        let template = RESPONSES
            .choose(&mut rand::rng())
            .expect("RESPONSES is non-empty");
        let message = template.replace("{beer}", &beer);

        Ok(CallToolResult::success(vec![Content::text(message)]))
    }
}

impl Default for BeerOrderService {
    fn default() -> Self {
        Self {
            required_role: String::new(),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_handler]
impl ServerHandler for BeerOrderService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_instructions(
            "Beer ordering service for SUSECON. \
             Call order_beer to place a beer order. \
             A human will deliver the beer to you after the order is confirmed.",
        )
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(3000);

    let required_role = std::env::var("REQUIRED_ROLE").unwrap_or_else(|_| "susecon-beer".into());
    let rancher_tls_verify = std::env::var("RANCHER_TLS_VERIFY")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);

    info!("Required role for beer ordering: \"{required_role}\"");
    info!("Rancher TLS verification: {}", if rancher_tls_verify { "enabled" } else { "DISABLED" });

    let bind_addr = format!("0.0.0.0:{port}");

    let ct = CancellationToken::new();

    let auth_state = RancherAuthState::new(rancher_tls_verify);

    let required_role_for_service = required_role.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(BeerOrderService::new(required_role_for_service.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_cancellation_token(ct.child_token())
            .disable_allowed_hosts(),
    );

    let mcp_router = Router::new()
        .fallback_service(mcp_service)
        .layer(middleware::from_fn_with_state(
            auth_state,
            rancher_auth_middleware,
        ));

    let app = Router::new()
        .route("/health", get(health))
        .nest("/mcp", mcp_router);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("🍺  susecon-beer-agent listening on {bind_addr}  (MCP endpoint: {bind_addr}/mcp)");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for ctrl-c");
            info!("Shutting down gracefully...");
            ct.cancel();
        })
        .await?;

    Ok(())
}

async fn health() -> &'static str {
    "OK"
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::handler::server::wrapper::Parameters;
    use rmcp::handler::server::common::Extension;

    // --- resolve_beer_name ---------------------------------------------------

    #[test]
    fn beer_name_from_none_defaults_to_beer() {
        assert_eq!(resolve_beer_name(None), "beer");
    }

    #[test]
    fn beer_name_from_empty_string_defaults_to_beer() {
        assert_eq!(resolve_beer_name(Some(String::new())), "beer");
    }

    #[test]
    fn beer_name_from_whitespace_defaults_to_beer() {
        assert_eq!(resolve_beer_name(Some("   ".to_string())), "beer");
    }

    #[test]
    fn beer_name_from_specified_type_is_preserved() {
        assert_eq!(resolve_beer_name(Some("IPA".to_string())), "IPA");
    }

    // --- RESPONSES constant --------------------------------------------------

    #[test]
    fn responses_is_non_empty() {
        assert!(!RESPONSES.is_empty());
    }

    #[test]
    fn all_response_templates_contain_placeholder() {
        for template in RESPONSES {
            assert!(
                template.contains("{beer}"),
                "template missing {{beer}} placeholder: {template}"
            );
        }
    }

    // --- helpers for tests ----------------------------------------------------

    fn make_authed_parts() -> http::request::Parts {
        let (mut parts, _) = http::Request::new(()).into_parts();
        parts.extensions.insert(AuthContext {
            display_name: "test-user".to_string(),
            roles: vec!["test-role".to_string()],
        });
        parts
    }

    // --- order_beer tool -----------------------------------------------------

    #[tokio::test(start_paused = true)]
    async fn order_beer_with_named_type_mentions_beer_in_response() {
        let svc = BeerOrderService::new("test-role".to_string());
        let result = svc
            .order_beer(
                Extension(make_authed_parts()),
                Parameters(OrderBeerParams {
                    beer_type: Some("stout".to_string()),
                }),
            )
            .await
            .expect("order_beer should succeed");

        assert!(result.is_error.is_none() || !result.is_error.unwrap());
        let text = result
            .content
            .iter()
            .filter_map(|c| c.as_text())
            .map(|t| t.text.as_str())
            .collect::<String>();
        assert!(text.contains("stout"), "response should mention the beer type: {text}");
    }

    #[tokio::test(start_paused = true)]
    async fn order_beer_with_no_type_defaults_to_beer() {
        let svc = BeerOrderService::new("test-role".to_string());
        let result = svc
            .order_beer(
                Extension(make_authed_parts()),
                Parameters(OrderBeerParams { beer_type: None }),
            )
            .await
            .expect("order_beer should succeed");

        assert!(result.is_error.is_none() || !result.is_error.unwrap());
        let text = result
            .content
            .iter()
            .filter_map(|c| c.as_text())
            .map(|t| t.text.as_str())
            .collect::<String>();
        assert!(text.contains("beer"), "response should contain 'beer': {text}");
    }

    // --- get_info ------------------------------------------------------------

    #[test]
    fn get_info_advertises_tools_capability() {
        let svc = BeerOrderService::new("test-role".to_string());
        let info = svc.get_info();
        assert!(
            info.capabilities.tools.is_some(),
            "server should advertise tools capability"
        );
    }
}
