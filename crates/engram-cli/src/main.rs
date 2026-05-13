//! engram — the only binary. Phase B wires up the `serve` and `migrate`
//! subcommands; `embed-backfill` lands in Phase D.

mod config;

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::Context;
use clap::{Parser, Subcommand};
use engram_core::{Embedder, EmbeddingModel};
use engram_embed::{OpenAICompatibleConfig, OpenAICompatibleEmbedder};
use engram_mcp::EngramServer;
use rmcp::transport::streamable_http_server::{
    StreamableHttpService, session::local::LocalSessionManager,
};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, EmbedderConfig};

#[derive(Parser, Debug)]
#[command(name = "engram", version, about = "Self-hosted MCP-native memory service")]
struct Cli {
    /// Path to an `engram.toml` config file. Overrides `~/.config/engram/engram.toml`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the MCP/HTTP server.
    Serve,
    /// Apply pending database migrations.
    Migrate,
    /// Embed thoughts that don't yet have an embedding row for the active model.
    EmbedBackfill {
        /// Restrict to a single scope.
        #[arg(long)]
        scope: Option<String>,
        /// Maximum number of thoughts to embed in this run. Defaults to 1000.
        #[arg(long, default_value_t = 1000)]
        limit: i64,
    },
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,engram=debug"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn build_embedder(c: &EmbedderConfig) -> anyhow::Result<Arc<dyn Embedder>> {
    match c.provider.as_str() {
        "openai-compatible" => {
            let embedder = OpenAICompatibleEmbedder::new(OpenAICompatibleConfig {
                endpoint: c.endpoint.clone(),
                model_name: c.model.clone(),
                model: EmbeddingModel::new(c.model_id.clone(), c.dimensions),
                api_key: c.api_key.clone(),
                timeout: Duration::from_secs(c.timeout_seconds),
            })
            .with_context(|| format!("constructing embedder for endpoint {}", c.endpoint))?;
            Ok(Arc::new(embedder))
        }
        other => anyhow::bail!(
            "unknown embedder provider: {other:?} (valid: 'openai-compatible')"
        ),
    }
}

async fn run_serve(config: Config) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;

    let embedder = build_embedder(&config.embedder)?;

    let bind: SocketAddr = config
        .server
        .bind
        .parse()
        .with_context(|| format!("parsing server.bind = {:?}", config.server.bind))?;

    let pool_for_factory = pool.clone();
    let embedder_for_factory = embedder.clone();
    let factory = move || Ok(EngramServer::new(pool_for_factory.clone(), embedder_for_factory.clone()));

    let cancel = CancellationToken::new();
    let mcp_service = StreamableHttpService::new(
        factory,
        LocalSessionManager::default().into(),
        Default::default(),
    );

    let app = axum::Router::new().nest_service("/mcp", mcp_service);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding HTTP server to {bind}"))?;

    tracing::info!(
        bind = %bind,
        embedder_endpoint = %config.embedder.endpoint,
        model_id = %config.embedder.model_id,
        "engram serve started"
    );

    let shutdown = {
        let cancel = cancel.clone();
        async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("shutdown signal received");
                }
                _ = cancel.cancelled() => {
                    tracing::info!("cancellation token tripped");
                }
            }
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("axum::serve")?;

    Ok(())
}

async fn run_migrate(config: Config) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    tracing::info!("migrations applied");
    Ok(())
}

async fn run_embed_backfill(
    config: Config,
    scope: Option<String>,
    limit: i64,
) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;

    let embedder = build_embedder(&config.embedder)?;

    let report =
        engram_mcp::embed_backfill(&pool, embedder.as_ref(), scope.as_deref(), limit).await?;

    tracing::info!(
        found = report.found,
        embedded = report.embedded,
        failed = report.failed,
        "backfill complete"
    );

    if report.failed > 0 {
        // Non-zero exit so scripts/cron can detect partial failures.
        anyhow::bail!(
            "{} of {} thoughts failed to embed (see logs); {} succeeded",
            report.failed,
            report.found,
            report.embedded
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let config = config::load(cli.config.as_deref()).context("loading config")?;

    match cli.command {
        Command::Serve => run_serve(config).await,
        Command::Migrate => run_migrate(config).await,
        Command::EmbedBackfill { scope, limit } => run_embed_backfill(config, scope, limit).await,
    }
}
