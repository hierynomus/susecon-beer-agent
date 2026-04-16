use std::time::Duration;

use anyhow::Result;
use axum::{Router, routing::get};
use rand::prelude::IndexedRandom;
use tracing_subscriber::prelude::*;
use rmcp::{
    ErrorData as McpError,
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
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
// MCP service
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct BeerOrderService {
    #[allow(dead_code)]
    tool_router: ToolRouter<BeerOrderService>,
}

#[tool_router]
impl BeerOrderService {
    pub fn new() -> Self {
        Self::default()
    }

    /// Order a beer from the bar. A human will deliver it to you shortly.
    #[tool(
        description = "Order a beer from the bar. A human operative will be dispatched \
                       to deliver it to you shortly after the order is placed."
    )]
    async fn order_beer(
        &self,
        Parameters(params): Parameters<OrderBeerParams>,
    ) -> Result<CallToolResult, McpError> {
        let beer = params
            .beer_type
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "beer".to_string());

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

    let bind_addr = format!("0.0.0.0:{port}");

    let ct = CancellationToken::new();

    let mcp_service = StreamableHttpService::new(
        || Ok(BeerOrderService::new()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token()),
    );

    let app = Router::new()
        .route("/health", get(health))
        .nest_service("/mcp", mcp_service);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    info!("🍺  beer-mcp listening on {bind_addr}  (MCP endpoint: {bind_addr}/mcp)");

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
