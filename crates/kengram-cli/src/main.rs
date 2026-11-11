//! kengram — the only binary. M1 wired `serve` and `migrate`; M2 added the
//! worker; M3 added `reflect`, `embed-backfill --target`, and the bench
//! harness. M4 collapses the facts pipeline into a thoughts-only sidecar:
//! `reflect` → `tag`, `embed-backfill` loses its `--target` flag, and the
//! worker drops its reflector cron in favour of a plain tag drainer that
//! runs on the same tick as the embed drainer.

mod backup;
mod bench;
mod config;
mod eval;

use std::{net::SocketAddr, path::PathBuf, str::FromStr, sync::Arc, time::Duration};

use anyhow::Context;
use clap::{Parser, Subcommand};
use kengram_core::{Embedder, EmbeddingModel, Tagger};
use kengram_embed::{
    OpenAICompatibleConfig, OpenAICompatibleEmbedder, Reranker, TeiReranker, TeiRerankerConfig,
};
use kengram_extract::{OpenAICompatibleConfig as TaggerConfigBuilder, OpenAICompatibleTagger};
use kengram_mcp::KengramServer;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, EmbedderConfig, RerankerConfig, TaggerConfig, WorkerConfig};

#[derive(Parser, Debug)]
#[command(
    name = "kengram",
    version,
    about = "Self-hosted MCP-native memory service"
)]
struct Cli {
    /// Path to an `kengram.toml` config file. Overrides `~/.config/kengram/kengram.toml`.
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
    /// Long-running worker process: drains both `pending_embeddings` and
    /// `pending_tags` on every `[worker] tick_interval_seconds` tick.
    /// Tag drainer only spawns when `[tagger]` is configured.
    Worker,
    /// Embed thoughts that don't yet have an embedding row for the active
    /// embedder model. M4 thoughts-only (the `--target` flag is gone).
    EmbedBackfill {
        /// Restrict to a single scope (exact match). Mutually exclusive
        /// with `--scope-prefix`.
        #[arg(long, conflicts_with = "scope_prefix")]
        scope: Option<String>,
        /// Restrict to scopes starting with this prefix (e.g. `kengram.`
        /// matches `kengram.dogfood`, `kengram.test`, etc.). Mutually
        /// exclusive with `--scope`.
        #[arg(long, conflicts_with = "scope")]
        scope_prefix: Option<String>,
        /// Maximum number of thoughts to embed in this run.
        #[arg(long, default_value_t = 1000)]
        limit: i64,
    },
    /// One-shot tag run. By default walks untagged thoughts (where
    /// `tags_extractor_version IS NULL`). With `--rerun`, also walks
    /// thoughts whose `tags_extractor_version < [tagger].model_version`
    /// (the tagger prompt has been bumped). With `--force`, re-tags every
    /// matching thought regardless of version — useful after switching the
    /// tagger model without bumping the prompt version.
    Tag {
        /// Restrict to a single scope (exact match). Mutually exclusive
        /// with `--scope-prefix`.
        #[arg(long, conflicts_with = "scope_prefix")]
        scope: Option<String>,
        /// Restrict to scopes starting with this prefix. Mutually
        /// exclusive with `--scope`.
        #[arg(long, conflicts_with = "scope")]
        scope_prefix: Option<String>,
        /// Max thoughts to process this run. Defaults to 200.
        #[arg(long, default_value_t = 200)]
        limit: i64,
        /// Re-tag thoughts whose `tags_extractor_version` is older than the
        /// configured `[tagger].model_version`. Without this, only
        /// never-tagged thoughts are walked.
        #[arg(long)]
        rerun: bool,
        /// Re-tag every matching thought regardless of version (re-stamps the
        /// configured model_version and records the new model_id). Use after
        /// switching the tagger model. Bound the run with --scope /
        /// --scope-prefix / --since / --limit.
        #[arg(long)]
        force: bool,
        /// Restrict to thoughts created at or after this RFC-3339 timestamp.
        /// Allowed with or without `--rerun` (unlike `kengram reflect`'s
        /// `--since`, which required `--rerun`).
        #[arg(long)]
        since: Option<String>,
        /// Before tagging, snapshot current tags + provenance for ALL
        /// non-retracted thoughts to a JSON file (retag overwrites `tags` in
        /// place; there is no history table). Bare `--snapshot` writes
        /// `./kengram-tag-snapshot-<unixtime>.json`; `--snapshot=PATH` writes
        /// to PATH. Recover by hand via psql if a retag produces worse tags.
        #[arg(long, value_name = "PATH", num_args = 0..=1, require_equals = true)]
        snapshot: Option<Option<PathBuf>>,
    },
    /// Re-file thoughts under a new scope (M7.2). Exactly one form:
    /// `--thought <uuid>` re-files one thought; `--scope <old>` renames a
    /// dotted subtree — `<old>` exactly plus `<old>.*` on dotted boundaries
    /// (`work.foobar` is never touched by `--scope work.foo`). Content is
    /// never modified; every move appends a row to the append-only
    /// `scope_changes` audit trail and re-enqueues tagging (live thoughts,
    /// when `[tagger]` is configured). `--reason` is required: one sentence
    /// on why the filing changed is the cheapest insurance for future
    /// archaeology. Rescopes are all-or-nothing; a subtree rename whose
    /// mapping overflows the 256-char scope cap rolls back completely.
    Rescope {
        /// Single-thought form: the thought's UUID. Mutually exclusive
        /// with `--scope`.
        #[arg(long, conflicts_with = "scope")]
        thought: Option<String>,
        /// Subtree form: source scope, matched exactly and on dotted
        /// boundaries below. Mutually exclusive with `--thought`.
        #[arg(long, conflicts_with = "thought")]
        scope: Option<String>,
        /// The new scope. Non-empty, max 256 chars. For the subtree form
        /// it may not be inside the renamed subtree (that rename would
        /// re-match its own output on every re-run).
        #[arg(long)]
        to: String,
        /// Required one-sentence reason, stored verbatim in every
        /// `scope_changes` audit row this run writes.
        #[arg(long)]
        reason: String,
        /// Free-text actor recorded in the audit trail (follows the
        /// `source` convention; e.g. `manual`, `ops:renaming`).
        #[arg(long, default_value = "manual")]
        actor: String,
        /// Print the plan (per-row old→new, match count, tag-requeue
        /// count) without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Benchmarking harness — A/B comparisons across search-pipeline
    /// configurations. Subcommand-action shape leaves room for additional
    /// bench targets without flattening the CLI.
    Bench {
        #[command(subcommand)]
        action: BenchAction,
    },
    /// Operator-diagnostics queries — surface state that's normally only
    /// visible via psql. Subcommand-action shape leaves room for more
    /// audit resources without flattening the CLI.
    Audit {
        #[command(subcommand)]
        resource: AuditResource,
    },
    /// Evaluation harness — offline quality measurement (first M7 eval
    /// suite: multi-model tagger comparison). `eval tagger` NEVER touches
    /// the database: it reads a corpus file, calls tagger HTTP endpoints,
    /// and writes a report file.
    Eval {
        #[command(subcommand)]
        action: eval::EvalAction,
    },
    /// Print corpus + storage telemetry: thought counts, embeddings,
    /// links, per-scope summary, per-table heap/index/total sizes.
    /// Operator-facing snapshot of "how much am I storing?" without psql.
    Stats {
        /// Optional scope prefix. Only filters which scopes appear in the
        /// scopes summary section; corpus-global counts stay global.
        #[arg(long)]
        scope_prefix: Option<String>,
        /// Limit the scopes summary tree to the N scopes with the most
        /// thoughts in their whole subtree (ancestors stay as structure).
        /// Defaults to 20.
        #[arg(long, default_value_t = 20)]
        top_scopes: usize,
    },
    /// Back up the database to a portable archive (pg_dump + manifest
    /// sidecar). Pair with `restore` for machine-to-machine migration.
    Backup {
        /// Output archive path. Defaults to
        /// `./kengram-backup-<timestamp>.tar.gz` in the current directory.
        #[arg(long, value_name = "PATH")]
        to: Option<PathBuf>,
        /// Exclude embeddings table data from the archive. Smaller
        /// backup; restore will require `kengram embed-backfill` to
        /// repopulate vectors. The HNSW index and table definition
        /// survive empty.
        #[arg(long)]
        skip_embeddings: bool,
    },
    /// Restore the database from a backup archive. DESTRUCTIVE — replaces
    /// existing schema and data on the target. Refuses without `--force`
    /// when the target already has thoughts. Validates the manifest
    /// against the target's schema head and embedder/tagger config before
    /// proceeding.
    Restore {
        /// Input archive path. Required.
        #[arg(long, value_name = "PATH")]
        from: PathBuf,
        /// Confirm replacement of existing data. Required when the
        /// target's `thoughts` table is non-empty; unnecessary on a
        /// freshly-migrated empty database.
        #[arg(long)]
        force: bool,
        /// Skip the manifest compatibility check (schema head, embedder,
        /// tagger). Advanced — use only when you understand the
        /// implications.
        #[arg(long)]
        skip_version_check: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AuditResource {
    /// Print the `migration_audit` log: per-migration ran_at, rows_touched,
    /// notes. Most recent first.
    Migrations {
        /// Optional RFC-3339 lower bound on `ran_at`. Restrict the log to
        /// recent entries.
        #[arg(long)]
        since: Option<String>,
        /// Maximum rows to print. Defaults to 50.
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
}

#[derive(Subcommand, Debug)]
enum BenchAction {
    /// Run rerank A/B (RRF-only vs cross-encoder-reranked) over a fixture
    /// corpus. Prints a markdown table to stdout. See
    /// `tests/fixtures/bench-rerank.example.json` for the fixture schema.
    /// Requires `[reranker]` to be configured.
    Rerank {
        /// Path to the fixture JSON file.
        #[arg(long)]
        corpus: PathBuf,
    },
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,kengram=debug"));
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
        other => anyhow::bail!("unknown embedder provider: {other:?} (valid: 'openai-compatible')"),
    }
}

/// Build the reranker per `[reranker]` config. Returns `None` when the
/// provider is empty (the silent-disable sentinel), so the search pipeline
/// falls through to the Phase B step 1 RRF + recency pipeline without
/// erroring. Logs the resolved config at INFO level — startup-config
/// observability convention from Phase A (commit 1d627e4).
fn build_reranker(c: &RerankerConfig) -> anyhow::Result<Option<Arc<dyn Reranker>>> {
    if c.provider.is_empty() {
        tracing::info!("reranker: not configured (rerank stage disabled)");
        return Ok(None);
    }
    match c.provider.as_str() {
        "tei" => {
            let reranker = TeiReranker::new(TeiRerankerConfig {
                endpoint: c.endpoint.clone(),
                model_id: c.model_id.clone(),
                timeout: Duration::from_secs(c.timeout_seconds),
                max_batch: c.max_batch,
            })
            .with_context(|| format!("constructing reranker for endpoint {}", c.endpoint))?;
            tracing::info!(
                provider = %c.provider,
                endpoint = %c.endpoint,
                model_id = %c.model_id,
                timeout_seconds = c.timeout_seconds,
                "reranker: resolved config",
            );
            Ok(Some(Arc::new(reranker)))
        }
        other => anyhow::bail!("unknown reranker provider: {other:?} (valid: 'tei' or empty)"),
    }
}

/// Resolved tagger plus the bits of the original config the callers need
/// to know about. `tagger` is `None` on silent-disable (empty `provider`);
/// the `version` field is the configured `model_version` either way so
/// `kengram tag` and the worker tick can compare against it without
/// re-reading config.
struct ResolvedTagger {
    tagger: Option<Arc<dyn Tagger>>,
    /// Stable identity the server stamps onto `pending_tags` rows at
    /// capture time. `None` mirrors `tagger == None`.
    model_id: Option<String>,
    /// Prompt/schema version, written into `thoughts.tags_extractor_version`.
    version: i32,
}

/// Build the tagger per `[tagger]` config. Returns a `ResolvedTagger` with
/// every field present on opt-in and only `version` populated on silent-
/// disable (empty `provider`). Mirrors `build_reranker`'s silent-disable
/// pattern. Logs the resolved config at INFO level on opt-in (Phase A
/// startup-config observability convention; commit 1d627e4).
fn build_tagger(c: &TaggerConfig) -> anyhow::Result<ResolvedTagger> {
    if c.provider.is_empty() {
        tracing::info!("tagger: not configured (capture-time enqueue silently disabled)");
        return Ok(ResolvedTagger {
            tagger: None,
            model_id: None,
            version: c.model_version,
        });
    }

    // Resolve the system prompt: bundled by default; load from a file when
    // `system_prompt_file` is set. The path is anyhow-context'd so errors
    // surface with the path the operator typed.
    let system_prompt =
        match c.system_prompt_file.as_ref() {
            Some(path) => Some(std::fs::read_to_string(path).with_context(|| {
                format!("reading tagger system_prompt_file at {}", path.display())
            })?),
            None => None,
        };
    tracing::info!(
        provider = %c.provider,
        endpoint = %c.endpoint,
        system_prompt = %match c.system_prompt_file.as_ref() {
            Some(p) => format!("file:{}", p.display()),
            None => "bundled".to_string(),
        },
        model_name = %c.model_name,
        model_id = %c.model_id,
        model_version = c.model_version,
        timeout_seconds = c.timeout_seconds,
        "tagger: resolved config",
    );

    // Provider registry. The `Tagger` trait at kengram-core is the public
    // contract for tagger backends — anyone who implements it is pluggable.
    // The match below is the registry of *known* implementations, not the
    // contract itself. To add a new in-tree backend:
    //   1. Write a struct implementing kengram_core::Tagger.
    //   2. Add a config sub-section to TaggerConfig if it needs config
    //      beyond the flat fields openai-compatible already uses.
    //   3. Add an arm to this match mapping a new provider string to your
    //      constructor.
    // For out-of-tree backends, prefer the sidecar pattern: run an HTTP
    // service that speaks kengram's wire contract (the kengram-tagger-protocol
    // crate is the spec) and use provider = "http" to point at it.
    // See `docs/tagger-backends.md` for the full contract + recipe.
    match c.provider.as_str() {
        "openai-compatible" | "openrouter" => {
            let tagger = OpenAICompatibleTagger::new(TaggerConfigBuilder {
                endpoint: c.endpoint.clone(),
                model_name: c.model_name.clone(),
                model_id: c.model_id.clone(),
                model_version: c.model_version,
                api_key: c.api_key.clone(),
                timeout: Duration::from_secs(c.timeout_seconds),
                temperature: c.temperature,
                system_prompt,
            })
            .with_context(|| format!("constructing tagger for endpoint {}", c.endpoint))?;
            Ok(ResolvedTagger {
                tagger: Some(Arc::new(tagger)),
                model_id: Some(c.model_id.clone()),
                version: c.model_version,
            })
        }
        "http" => {
            let http_cfg = c.http.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "provider = \"http\" requires a [tagger.http] config section (endpoint, optional api_key, optional timeout_seconds). See docs/tagger-backends.md.",
                )
            })?;
            let tagger = kengram_extract::HttpTagger::new(kengram_extract::HttpTaggerConfig {
                endpoint: http_cfg.endpoint.clone(),
                model_id: c.model_id.clone(),
                model_version: c.model_version,
                api_key: http_cfg.api_key.clone(),
                timeout: Duration::from_secs(http_cfg.timeout_seconds),
            })
            .with_context(|| {
                format!(
                    "constructing http tagger for endpoint {}",
                    http_cfg.endpoint
                )
            })?;
            Ok(ResolvedTagger {
                tagger: Some(Arc::new(tagger)),
                model_id: Some(c.model_id.clone()),
                version: c.model_version,
            })
        }
        other => anyhow::bail!(
            "unknown tagger provider: {other:?} (valid: 'openai-compatible', 'openrouter', 'http' for sidecar, or empty for silent-disable). See docs/tagger-backends.md for how to register a new backend."
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
    let reranker = build_reranker(&config.reranker)?;
    // Server only needs the tagger model_id (to stamp pending_tags rows).
    // The actual tagger HTTP client lives in the worker process.
    let ResolvedTagger {
        tagger: _,
        model_id: tagger_model_id,
        version: _,
    } = build_tagger(&config.tagger)?;

    let bind: SocketAddr = config
        .server
        .bind
        .parse()
        .with_context(|| format!("parsing server.bind = {:?}", config.server.bind))?;

    let pool_for_factory = pool.clone();
    let embedder_for_factory = embedder.clone();
    let reranker_for_factory = reranker.clone();
    let tagger_model_id_for_factory = tagger_model_id.clone();
    let factory = move || {
        Ok(KengramServer::new(
            pool_for_factory.clone(),
            embedder_for_factory.clone(),
            reranker_for_factory.clone(),
            tagger_model_id_for_factory.clone(),
        ))
    };

    let cancel = CancellationToken::new();
    // Stateless mode (allowed by MCP Streamable HTTP spec 2025-06-18 for
    // simple request-response tools). Kengram has no per-session state — every
    // tool is `Result<Response, Error>`, returns synchronously, doesn't push
    // events. Disabling stateful mode means rmcp never issues a session id,
    // never runs the idle-session reaper, and therefore can't return
    // `Session not found` 404s when a long-lived MCP client (Claude Desktop,
    // mcp-remote bridge) comes back after idling past the 5-minute default.
    // `json_response: true` pairs naturally — replies are plain JSON, no SSE
    // framing overhead.
    // DNS-rebinding protection: rmcp ships with a safe default allowlist
    // (`localhost` / `127.0.0.1` / `::1`). Operators binding to a non-loopback
    // interface (Tailnet, LAN) must extend the allowlist via
    // `[server].allowed_hosts` in kengram.toml, or the rmcp transport rejects
    // every request whose Host header isn't `localhost`. Empty config list =
    // keep rmcp's default; non-empty replaces it.
    let mut http_cfg = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true);
    if !config.server.allowed_hosts.is_empty() {
        http_cfg = http_cfg.with_allowed_hosts(config.server.allowed_hosts.clone());
    }
    let mcp_service =
        StreamableHttpService::new(factory, LocalSessionManager::default().into(), http_cfg);

    let mut app = axum::Router::new().nest_service("/mcp", mcp_service);
    // M8: mount the read-only human web surface alongside `/mcp` when enabled.
    // Shares the same pool/embedder/reranker; never mutates. Its routes get a
    // Host-header guard mirroring the rmcp `allowed_hosts` check on `/mcp`.
    if config.web.enabled {
        app = app.merge(kengram_web::router(kengram_web::WebState {
            pool: pool.clone(),
            embedder: embedder.clone(),
            reranker: reranker.clone(),
            allowed_hosts: config.server.allowed_hosts.clone(),
        }));
        tracing::info!("web: read-only UI mounted at / and /api (M8)");
    }
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding HTTP server to {bind}"))?;

    let allowed_hosts_summary = if config.server.allowed_hosts.is_empty() {
        "rmcp default (localhost / 127.0.0.1 / ::1)".to_string()
    } else {
        format!("custom: {:?}", config.server.allowed_hosts)
    };
    tracing::info!(
        bind = %bind,
        allowed_hosts = %allowed_hosts_summary,
        embedder_endpoint = %config.embedder.endpoint,
        model_id = %config.embedder.model_id,
        tagger = %match tagger_model_id.as_deref() {
            Some(id) => format!("enabled ({id})"),
            None => "disabled".to_string(),
        },
        "kengram serve started"
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

async fn run_worker(config: Config) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;
    let embedder = build_embedder(&config.embedder)?;
    let ResolvedTagger {
        tagger,
        model_id: tagger_model_id,
        version: _,
    } = build_tagger(&config.tagger)?;

    let cancel = CancellationToken::new();
    let mut set = tokio::task::JoinSet::new();

    let WorkerConfig {
        tick_interval_seconds,
        batch_size,
    } = config.worker;
    let interval = Duration::from_secs(tick_interval_seconds);

    let drain_pool = pool.clone();
    let drain_embedder = embedder.clone();
    let drain_cancel = cancel.clone();
    set.spawn(async move {
        embed_drainer_loop(
            drain_pool,
            drain_embedder,
            interval,
            batch_size,
            drain_cancel,
        )
        .await;
    });

    // Scope-vocabulary injection: when enabled and size > 0, the drainer
    // pre-fetches the top-N established terms in each thought's scope and
    // passes them to the tagger as controlled-vocabulary hints.
    let scope_vocab_limit: Option<i64> =
        if config.tagger.scope_vocab_enabled && config.tagger.scope_vocab_size > 0 {
            Some(i64::from(config.tagger.scope_vocab_size))
        } else {
            None
        };

    // Tag drainer is silent-disabled when [tagger] isn't configured —
    // mirrors the capture-side enqueue gate. No tag rows can exist in the
    // queue if no captures enqueued them, so even spinning the loop would
    // be wasted ticks; just don't spawn it.
    let tagger_summary = match tagger {
        Some(t) => {
            let tag_pool = pool.clone();
            let tag_tagger = t.clone();
            let tag_cancel = cancel.clone();
            let model_id = t.model_id().to_string();
            set.spawn(async move {
                tag_drainer_loop(
                    tag_pool,
                    tag_tagger,
                    interval,
                    batch_size,
                    scope_vocab_limit,
                    tag_cancel,
                )
                .await;
            });
            format!("enabled ({model_id})")
        }
        None => "disabled".to_string(),
    };

    tracing::info!(
        tick_interval_seconds,
        batch_size,
        embedder_endpoint = %config.embedder.endpoint,
        model_id = %config.embedder.model_id,
        tagger = %tagger_summary,
        tagger_model_id = ?tagger_model_id,
        scope_vocab_limit = ?scope_vocab_limit,
        "kengram worker started"
    );

    // Wait for ctrl-c, then signal the loop(s) to wind down and join with a
    // 30s ceiling so a hung embed call can't block shutdown forever.
    tokio::signal::ctrl_c()
        .await
        .context("waiting for shutdown signal")?;
    tracing::info!("shutdown signal received");
    cancel.cancel();

    let shutdown_deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::select! {
        _ = async { while set.join_next().await.is_some() {} } => {
            tracing::info!("worker tasks exited cleanly");
        }
        _ = shutdown_deadline => {
            tracing::warn!("worker tasks did not exit within 30s; forcing exit");
            set.abort_all();
        }
    }
    Ok(())
}

async fn embed_drainer_loop(
    pool: sqlx::PgPool,
    embedder: std::sync::Arc<dyn Embedder>,
    interval: Duration,
    batch_size: i64,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    // Skip the eager first tick that `interval` fires immediately so we wait
    // a full interval before the first drain.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("embed drainer shutting down");
                return;
            }
            _ = ticker.tick() => {
                match kengram_mcp::drain_pending_embeddings(&pool, embedder.as_ref(), batch_size).await {
                    Ok(report) if report.found > 0 => tracing::info!(
                        found = report.found,
                        embedded = report.embedded,
                        failed = report.failed,
                        "embed drain tick",
                    ),
                    Ok(_) => {} // idle tick; stay quiet
                    Err(err) => tracing::error!(error = ?err, "embed drain tick failed"),
                }
            }
        }
    }
}

/// Tag drainer mirror of `embed_drainer_loop`. Same cadence (`[worker]
/// tick_interval_seconds`) + batch size (`[worker] batch_size`) — Q1 of
/// the M4 spec settled on "one cadence number for the operator to reason
/// about." Only spawned when `[tagger]` is configured.
async fn tag_drainer_loop(
    pool: sqlx::PgPool,
    tagger: std::sync::Arc<dyn Tagger>,
    interval: Duration,
    batch_size: i64,
    scope_vocab_limit: Option<i64>,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!("tag drainer shutting down");
                return;
            }
            _ = ticker.tick() => {
                match kengram_mcp::drain_pending_tags(
                    &pool,
                    tagger.as_ref(),
                    batch_size,
                    scope_vocab_limit,
                )
                .await
                {
                    Ok(report) if report.processed > 0 => tracing::info!(
                        processed = report.processed,
                        completed = report.completed,
                        failed_transient = report.failed_transient,
                        failed_permanent = report.failed_permanent,
                        "tag drain tick",
                    ),
                    Ok(_) => {} // idle tick; stay quiet
                    Err(err) => tracing::error!(error = ?err, "tag drain tick failed"),
                }
            }
        }
    }
}

async fn run_embed_backfill(
    config: Config,
    scope: Option<String>,
    scope_prefix: Option<String>,
    limit: i64,
) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;

    let embedder = build_embedder(&config.embedder)?;

    // Treat `--scope ""` / `--scope-prefix ""` as "no filter" (same
    // empty-string normalisation applied elsewhere on the config side).
    let scope_filter = scope.filter(|s| !s.is_empty());
    let scope_prefix_filter = scope_prefix.filter(|s| !s.is_empty());

    let report = kengram_mcp::embed_backfill(
        &pool,
        embedder.as_ref(),
        scope_filter.as_deref(),
        scope_prefix_filter.as_deref(),
        limit,
    )
    .await?;

    tracing::info!(
        healed = report.healed,
        embedded = report.embedded,
        failed = report.failed,
        "backfill complete"
    );

    if report.failed > 0 {
        // Non-zero exit so scripts/cron can detect partial failures.
        anyhow::bail!(
            "{} thoughts failed to embed (see logs); {} succeeded",
            report.failed,
            report.embedded
        );
    }
    Ok(())
}

/// `kengram tag` — one-shot tag run. Mirrors `kengram reflect`'s M3 surface:
/// walks the candidate set returned by
/// `kengram_storage::find_untagged_or_stale_thoughts`, calls
/// `tagger.tag(content)` on each, persists via `update_thought_tags`. No
/// queue interaction — `pending_tags` is the worker-tick path, not this
/// one. Hard-fails when `[tagger]` is unconfigured (matches `kengram bench
/// rerank`'s "configured reranker required" stance).
/// Shape pre-retag snapshot rows into the JSON array written by `--snapshot`.
/// `thought_id` serializes as a bare UUID string (`ThoughtId` is transparent),
/// and untagged rows carry `null` provenance — both pinned by the unit test.
fn snapshot_rows_to_json(rows: &[kengram_storage::TagSnapshotRow]) -> Vec<serde_json::Value> {
    rows.iter()
        .map(|r| {
            serde_json::json!({
                "thought_id": r.thought_id,
                "tags": r.tags,
                "tags_extractor_model": r.tags_extractor_model,
                "tags_extractor_version": r.tags_extractor_version,
            })
        })
        .collect()
}

// Args mirror the `Command::Tag` clap flags 1:1; grouping them into a struct
// would just move the same fields behind one more name. Same rationale as the
// wide query helpers in kengram-storage.
#[allow(clippy::too_many_arguments)]
async fn run_tag(
    config: Config,
    scope: Option<String>,
    scope_prefix: Option<String>,
    limit: i64,
    rerun: bool,
    force: bool,
    since: Option<String>,
    snapshot: Option<Option<PathBuf>>,
) -> anyhow::Result<()> {
    let parsed_since = match since {
        Some(s) => Some(
            time::OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
                .with_context(|| format!("parsing --since={s:?} as RFC-3339"))?,
        ),
        None => None,
    };

    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;

    // Pre-retag safety net: snapshot current tags before any destructive
    // overwrite. Runs before build_tagger so the snapshot is captured even if
    // the tagger turns out to be misconfigured. Corpus-wide (all non-retracted
    // rows) regardless of --scope/--since, so any row stays recoverable.
    if let Some(snapshot_arg) = snapshot {
        let path = snapshot_arg.unwrap_or_else(|| {
            PathBuf::from(format!(
                "kengram-tag-snapshot-{}.json",
                time::OffsetDateTime::now_utc().unix_timestamp()
            ))
        });
        let rows = kengram_storage::snapshot_nonretracted_tags(&pool)
            .await
            .context("capturing pre-retag tag snapshot")?;
        let json = snapshot_rows_to_json(&rows);
        let file = std::fs::File::create(&path)
            .with_context(|| format!("creating snapshot file {}", path.display()))?;
        serde_json::to_writer_pretty(file, &json)
            .with_context(|| format!("writing snapshot to {}", path.display()))?;
        tracing::info!(
            count = rows.len(),
            path = %path.display(),
            "kengram tag: wrote pre-retag tag snapshot",
        );
        println!(
            "Wrote pre-retag snapshot of {} non-retracted thoughts to {}",
            rows.len(),
            path.display()
        );
    }

    let ResolvedTagger {
        tagger,
        model_id: _,
        version: tagger_version,
    } = build_tagger(&config.tagger)?;
    let tagger = tagger
        .context("`kengram tag` requires a configured `[tagger]` section; see DEVELOPMENT.md")?;

    // Mirror the worker's scope-vocab resolution so one-shot tagging applies
    // the same controlled-vocabulary behavior the drainer would.
    let scope_vocab_limit: Option<i64> =
        if config.tagger.scope_vocab_enabled && config.tagger.scope_vocab_size > 0 {
            Some(i64::from(config.tagger.scope_vocab_size))
        } else {
            None
        };

    // Treat `--scope ""` / `--scope-prefix ""` as "no filter" (matches the
    // empty-string-as-None normalisation applied elsewhere).
    let scope_filter = scope.filter(|s| !s.is_empty());
    let scope_prefix_filter = scope_prefix.filter(|s| !s.is_empty());

    tracing::info!(
        scope = ?scope_filter,
        scope_prefix = ?scope_prefix_filter,
        limit,
        rerun,
        force,
        since = ?parsed_since,
        target_version = tagger_version,
        scope_vocab_limit = ?scope_vocab_limit,
        "kengram tag starting",
    );

    let candidates = kengram_storage::find_untagged_or_stale_thoughts(
        &pool,
        tagger_version,
        rerun,
        force,
        scope_filter.as_deref(),
        scope_prefix_filter.as_deref(),
        parsed_since,
        limit,
    )
    .await
    .context("walking untagged-or-stale thoughts")?;

    let n_candidates = candidates.len();
    let mut tagged = 0usize;
    let mut failed = 0usize;
    let model_id = tagger.model_id().to_string();

    // Corpus scope set, fetched once (mirrors the worker drainer) so the
    // scope-identifier filter in `finalize` runs without a per-thought query.
    let known_scopes = kengram_storage::list_scopes(&pool, None)
        .await
        .map(|s| {
            s.into_iter()
                .map(|x| x.scope.as_str().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for t in candidates {
        let vocab = match scope_vocab_limit {
            Some(n) if n > 0 => {
                match kengram_storage::fetch_scope_vocab(&pool, t.scope.as_str(), n).await {
                    Ok(v) if v.is_empty() => None,
                    Ok(v) => Some(v),
                    Err(err) => {
                        tracing::warn!(
                            thought_id = %t.id,
                            scope = %t.scope.as_str(),
                            error = ?err,
                            "kengram tag: scope vocab fetch failed; tagging without vocab",
                        );
                        None
                    }
                }
            }
            _ => None,
        };
        match tagger.tag(&t.content, vocab.as_ref()).await {
            Ok(mut output) => {
                // Apply the same deterministic post-tag pipeline the worker
                // drainer runs (topic-normalize + people/entities disjoint).
                // This path previously skipped it, so CLI-tagged rows could
                // diverge from worker-tagged rows for the same thought.
                kengram_mcp::finalize::finalize_tags(
                    &mut output.tags,
                    &t.metadata,
                    &t.scope,
                    vocab.as_ref(),
                    &known_scopes,
                );
                if let Err(err) = kengram_storage::update_thought_tags(
                    &pool,
                    t.id,
                    &output.tags,
                    &model_id,
                    tagger_version,
                )
                .await
                {
                    tracing::warn!(
                        thought_id = %t.id,
                        error = ?err,
                        "kengram tag: storage write failed; continuing",
                    );
                    failed += 1;
                } else {
                    // Apply tagger-extracted relations after tags persist.
                    // Mirrors the worker drainer path so synchronous
                    // `kengram tag` and async drainer behave identically.
                    // Relations go to thought_links via apply_tagger_relations,
                    // NOT into tags.relations (migration 0011 removed the
                    // JSONB key).
                    kengram_mcp::apply_tagger_relations(&pool, t.id, &output.relations).await;
                    tagged += 1;
                }
            }
            Err(err) => {
                tracing::warn!(
                    thought_id = %t.id,
                    error = ?err,
                    "kengram tag: tagger call failed; continuing",
                );
                failed += 1;
            }
        }
    }

    tracing::info!(n_candidates, tagged, failed, "kengram tag complete",);

    if failed > 0 {
        anyhow::bail!(
            "{} thoughts failed tagging (see logs); {} tagged successfully",
            failed,
            tagged,
        );
    }
    Ok(())
}

// Re-file thoughts (M7.2). Thin surface: parse → pool → orchestrator.
// All semantics (guards, validation, all-or-nothing, audit, requeue)
// live in `kengram_mcp::rescope` so the CLI and MCP tool can't diverge.
async fn run_rescope(
    config: Config,
    thought: Option<String>,
    scope: Option<String>,
    to: String,
    reason: String,
    actor: String,
    dry_run: bool,
) -> anyhow::Result<()> {
    if thought.is_none() && scope.is_none() {
        anyhow::bail!("one of --thought <uuid> or --scope <old> is required");
    }

    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;

    // Same silent-disable rule as `build_tagger` — rescope needs the model
    // id only, not a tagger client.
    let tagger_model_id = if config.tagger.provider.is_empty() {
        tracing::warn!(
            "tagger: not configured — rescopes will move and audit but enqueue no re-tag jobs"
        );
        None
    } else {
        Some(config.tagger.model_id.clone())
    };

    match (thought, scope) {
        (Some(id_raw), None) => {
            let thought_id = kengram_core::ThoughtId::from_str(&id_raw)
                .with_context(|| format!("parsing --thought={id_raw:?} as UUID"))?;
            // Both --dry-run and a real run go through the orchestrator —
            // same guards, same pre-read, same plan. An early read-only
            // preview here could print "would re-file" for inputs the
            // real run then rejects, and could only guess the requeue
            // count. The orchestrator's dry run rolls back after the
            // locking pre-read instead: accurate preview, zero writes.
            let resp = kengram_mcp::rescope_thought(
                &pool,
                tagger_model_id.as_deref(),
                kengram_mcp::RescopeThoughtRequest {
                    thought_id,
                    scope: to,
                    reason,
                    actor: Some(actor),
                    dry_run,
                },
            )
            .await
            .with_context(|| format!("rescoping thought {thought_id}"))?;
            match (resp.changed, resp.committed) {
                (true, true) => {
                    println!(
                        "rescoped {thought_id}: {:?} -> {:?}",
                        resp.previous_scope, resp.new_scope
                    );
                    if resp.requeued {
                        println!("  tag job re-enqueued — the drainer re-tags under the new scope");
                    } else {
                        println!("  no tag requeue (retracted thought or [tagger] not configured)");
                    }
                }
                (true, false) => {
                    println!(
                        "dry run: would re-file {thought_id}: {:?} -> {:?}",
                        resp.previous_scope, resp.new_scope
                    );
                    println!(
                        "  tag requeue: {}",
                        if resp.requeued {
                            "1 (live thought)"
                        } else {
                            "0 (retracted thought or [tagger] not configured)"
                        }
                    );
                }
                (false, _) => {
                    println!(
                        "no-op: {thought_id} already filed under {:?}{}",
                        resp.previous_scope,
                        if dry_run {
                            " (dry run — nothing written)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        (None, Some(from)) => {
            let resp = kengram_mcp::rename_scope_subtree(
                &pool,
                tagger_model_id.as_deref(),
                kengram_mcp::RenameScopeRequest {
                    from: from.clone(),
                    to,
                    reason,
                    actor: Some(actor),
                    dry_run,
                },
            )
            .await
            .with_context(|| format!("renaming scope subtree {from:?}"))?;
            for row in &resp.moved {
                println!(
                    "  {} : {:?} -> {:?}",
                    row.thought_id, row.old_scope, row.new_scope
                );
            }
            if resp.moved.is_empty() {
                println!("no-op: nothing filed under {from:?} or its subtree");
            } else if dry_run {
                println!(
                    "dry run: {} matched, {} would requeue tagging — nothing written",
                    resp.moved.len(),
                    resp.requeued
                );
            } else {
                println!(
                    "{} thoughts re-filed; {} tag jobs queued",
                    resp.moved.len(),
                    resp.requeued
                );
            }
            if resp.requeued >= 100 {
                // Queue cost (m7.2 contract): requeues land ahead of every
                // later capture — oldest-first claim. Concrete on the live
                // corpus: rjf.tech ≈ 276 thoughts ≈ 27 h at CPU p50.
                println!(
                    "warning: {} re-tag jobs park a backlog ahead of later captures — \
                     consider timing this before the nightly GPU tagger window (docs/linux-autostart.md)",
                    resp.requeued
                );
            }
        }
        // Both arms below were validated before the pool connect; clap
        // rejects the both-present case at parse time.
        (Some(_), Some(_)) | (None, None) => {
            unreachable!("validated above: exactly one of --thought/--scope")
        }
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
        Command::Worker => run_worker(config).await,
        Command::EmbedBackfill {
            scope,
            scope_prefix,
            limit,
        } => run_embed_backfill(config, scope, scope_prefix, limit).await,
        Command::Tag {
            scope,
            scope_prefix,
            limit,
            rerun,
            force,
            since,
            snapshot,
        } => {
            run_tag(
                config,
                scope,
                scope_prefix,
                limit,
                rerun,
                force,
                since,
                snapshot,
            )
            .await
        }
        Command::Rescope {
            thought,
            scope,
            to,
            reason,
            actor,
            dry_run,
        } => run_rescope(config, thought, scope, to, reason, actor, dry_run).await,
        Command::Bench { action } => match action {
            BenchAction::Rerank { corpus } => run_bench_rerank(config, corpus).await,
        },
        Command::Audit { resource } => match resource {
            AuditResource::Migrations { since, limit } => {
                run_audit_migrations(config, since, limit).await
            }
        },
        Command::Eval { action } => match action {
            // Deliberately does NOT receive `config`: `eval tagger` must
            // never see a database URL, let alone connect (see eval/mod.rs).
            eval::EvalAction::Tagger(args) => eval::run_tagger_cli(args).await,
            eval::EvalAction::ExportCorpus(args) => {
                eval::export::run_export_cli(config, args).await
            }
        },
        Command::Stats {
            scope_prefix,
            top_scopes,
        } => run_stats(config, scope_prefix, top_scopes).await,
        Command::Backup {
            to,
            skip_embeddings,
        } => backup::run_backup(config, to, skip_embeddings).await,
        Command::Restore {
            from,
            force,
            skip_version_check,
        } => backup::run_restore(config, from, force, skip_version_check).await,
    }
}

async fn run_audit_migrations(
    config: Config,
    since: Option<String>,
    limit: i64,
) -> anyhow::Result<()> {
    let parsed_since = match since {
        Some(s) => Some(
            time::OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
                .with_context(|| format!("parsing --since={s:?} as RFC-3339"))?,
        ),
        None => None,
    };

    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;

    let rows = kengram_storage::query_migration_audit(&pool, parsed_since, limit)
        .await
        .context("querying migration_audit")?;

    if rows.is_empty() {
        println!("(no migration_audit rows)");
        return Ok(());
    }

    // Two-line-per-row "header + notes" rendering. Migration names are
    // long; a column-table would either truncate them or fan out wide.
    for r in rows {
        let ran_at = r
            .ran_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| r.ran_at.to_string());
        println!(
            "{ran_at}  rows_touched={rows}  {name}",
            ran_at = ran_at,
            rows = r.rows_touched,
            name = r.migration,
        );
        if let Some(notes) = r.notes {
            println!("    {notes}");
        }
    }
    Ok(())
}

async fn run_bench_rerank(config: Config, corpus: PathBuf) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;

    let embedder = build_embedder(&config.embedder)?;
    let reranker = build_reranker(&config.reranker)?
        .context("bench rerank requires a configured [reranker] section; see DEVELOPMENT.md")?;
    bench::run_bench_rerank(&pool, embedder, reranker, &corpus).await
}

/// `kengram stats` — corpus + storage telemetry snapshot. Read-only; reads
/// from the live DB and prints a sectional plain-text report. Designed for
/// "operator wants to know what's stored" without a psql session.
async fn run_stats(
    config: Config,
    scope_prefix: Option<String>,
    top_scopes: usize,
) -> anyhow::Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database.max_connections)
        .connect(&config.database.url)
        .await
        .with_context(|| format!("connecting to {}", config.database.url))?;

    let scope_prefix = scope_prefix.filter(|s| !s.is_empty());
    let stats = kengram_storage::corpus_stats(&pool, scope_prefix.as_deref())
        .await
        .context("querying corpus_stats")?;

    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "<now>".into());
    println!("kengram corpus — {now}");
    println!();

    // -- Corpus section --
    let retracted_pct = if stats.thoughts.live + stats.thoughts.retracted > 0 {
        100 * stats.thoughts.retracted / (stats.thoughts.live + stats.thoughts.retracted)
    } else {
        0
    };
    println!(
        "  Thoughts:    {} live, {} retracted ({}%), {} untagged",
        stats.thoughts.live, stats.thoughts.retracted, retracted_pct, stats.thoughts.untagged
    );
    println!(
        "  Content:     {} total (avg {}/thought)",
        humanize_bytes(stats.thoughts.content_bytes_total),
        humanize_bytes(stats.thoughts.content_bytes_avg)
    );
    if stats.embeddings.is_empty() {
        println!("  Embeddings:  none");
    } else {
        let parts: Vec<String> = stats
            .embeddings
            .iter()
            .map(|e| {
                format!(
                    "{} × {} ({}-dim, v{})",
                    e.count, e.model_id, e.dimensions, e.model_version
                )
            })
            .collect();
        println!("  Embeddings:  {}", parts.join("; "));
    }
    println!(
        "  Links:       {} live, {} soft-deleted",
        stats.links.live, stats.links.soft_deleted
    );
    if !stats.links.by_relation.is_empty() {
        let parts: Vec<String> = stats
            .links
            .by_relation
            .iter()
            .map(|(k, n)| format!("{k} {n}"))
            .collect();
        println!("    by relation:   {}", parts.join(", "));
    }
    if !stats.links.by_kind.is_empty() {
        let parts: Vec<String> = stats
            .links
            .by_kind
            .iter()
            .map(|(k, n)| format!("{k} {n}"))
            .collect();
        println!("    by kind:       {}", parts.join(", "));
    }
    if !stats.links.by_source.is_empty() {
        let parts: Vec<String> = stats
            .links
            .by_source
            .iter()
            .map(|(k, n)| format!("{k} {n}"))
            .collect();
        println!("    by source:     {}", parts.join(", "));
    }
    println!(
        "  Queues:      embeddings: {} pending, tags: {} pending",
        stats.queues.pending_embeddings, stats.queues.pending_tags
    );

    // -- Scopes section --
    println!();
    for line in render_scopes_tree(&stats.scopes, top_scopes, scope_prefix.as_deref()) {
        println!("{line}");
    }

    // -- On-disk tables section --
    println!();
    println!(
        "On-disk tables ({} total):",
        humanize_bytes(stats.database_total_bytes)
    );
    let max_table_w = stats
        .storage
        .iter()
        .map(|t| t.table.len())
        .max()
        .unwrap_or(0);
    for t in &stats.storage {
        println!(
            "  {table:<width$}  {total:>9}  (heap {heap}, indexes {idx})",
            table = t.table,
            width = max_table_w,
            total = humanize_bytes(t.total_bytes),
            heap = humanize_bytes(t.heap_bytes),
            idx = humanize_bytes(t.indexes_bytes),
        );
    }

    Ok(())
}

/// Format a byte count as a human-readable string using kibi-base units
/// (1 KB = 1024 bytes; matches what operators see in `du -h` / `psql`'s
/// `pg_size_pretty`). One decimal place for MB+ to keep precision useful
/// at small corpus sizes without overwhelming the eye.
fn humanize_bytes(n: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let n_f = n as f64;
    if n_f < KB {
        format!("{n} B")
    } else if n_f < MB {
        format!("{:.0} KB", n_f / KB)
    } else if n_f < GB {
        format!("{:.1} MB", n_f / MB)
    } else {
        format!("{:.2} GB", n_f / GB)
    }
}

// -- Scopes tree rendering (presentation only; used by run_stats) ---------
//
// `kengram_storage::list_scopes` returns a flat list of *populated* scopes
// (exact-scope counts, live thoughts only). The Scopes section rebuilds the
// dotted hierarchy from it: alphabetical siblings at every level, ancestors
// that no thought sits at directly (`rjf`, `rjf.professional`) synthesized as
// branch nodes, and each line's headline number rolled up across the whole
// subtree, with the node's own direct count in parens when smaller.

/// Trie node over dotted scope segments. `own_count` is `Some` only for
/// populated scopes; synthesized ancestors are branch nodes.
#[derive(Default)]
struct ScopeTreeNode {
    own_count: Option<i64>,
    own_last: Option<time::OffsetDateTime>,
    children: std::collections::BTreeMap<String, ScopeTreeNode>,
}

impl ScopeTreeNode {
    /// Thoughts in this node's whole subtree (own + descendants).
    fn rollup(&self) -> i64 {
        self.own_count.unwrap_or(0) + self.children.values().map(Self::rollup).sum::<i64>()
    }

    /// Populated scopes in this node's whole subtree.
    fn populated(&self) -> i64 {
        (if self.own_count.is_some() { 1 } else { 0 })
            + self.children.values().map(Self::populated).sum::<i64>()
    }

    /// Latest activity in the subtree (own + descendants).
    fn last(&self) -> Option<time::OffsetDateTime> {
        self.own_last
            .max(self.children.values().filter_map(Self::last).max())
    }
}

/// Full scope path for a child segment; roots join onto the empty path
/// without a leading dot.
fn scope_child_path(path: &str, seg: &str) -> String {
    if path.is_empty() {
        seg.to_string()
    } else {
        format!("{path}.{seg}")
    }
}

/// Walk the trie collecting `(full scope path, subtree rollup)` for every
/// populated node.
fn collect_scope_paths(node: &ScopeTreeNode, path: &str, out: &mut Vec<(String, i64)>) {
    if node.own_count.is_some() {
        out.push((path.to_string(), node.rollup()));
    }
    for (seg, child) in &node.children {
        collect_scope_paths(child, &scope_child_path(path, seg), out);
    }
}

/// A node renders if it was selected itself or any descendant was — the
/// ancestor closure keeps pruned leaves attached to visible parents.
fn scope_node_shown(
    node: &ScopeTreeNode,
    path: &str,
    kept: &std::collections::HashSet<String>,
) -> bool {
    kept.contains(path)
        || node
            .children
            .iter()
            .any(|(seg, c)| scope_node_shown(c, &scope_child_path(path, seg), kept))
}

/// One rendered line's raw pieces; column widths are applied in a second
/// pass once every line is known.
enum ScopeLineKind {
    Node {
        /// Subtree rollup; own count when strictly smaller than it.
        rollup: i64,
        own: Option<i64>,
        last: String,
        /// No thought sits at this exact scope (synthesized ancestor).
        branch_only: bool,
    },
    /// Collapsed pruned siblings: `… (+M scopes, K thoughts)`.
    Hidden { scopes: i64, thoughts: i64 },
}

struct ScopeLine {
    prefix: String,
    name: String,
    kind: ScopeLineKind,
}

/// Render the whole Scopes section (header + tree lines) from the flat
/// scope list. `top` selects the N populated scopes with the **largest
/// subtree rollups** (ties: scope string asc); pruned scopes stay counted
/// in ancestor rollups and are surfaced per level via a
/// `… (+M scopes, K thoughts)` line plus the header's
/// `(showing top N, M more hidden)` note. Nodes forced in only to keep a
/// pruned leaf attached to a visible parent are structure, not hidden
/// scopes.
fn render_scopes_tree(
    scopes: &[kengram_storage::ScopeSummary],
    top: usize,
    scope_prefix: Option<&str>,
) -> Vec<String> {
    let suffix = scope_prefix
        .filter(|p| !p.is_empty())
        .map(|p| format!(" matching prefix {p:?}"))
        .unwrap_or_default();
    if scopes.is_empty() {
        return vec![format!("Scopes (0){suffix}: (no scopes)")];
    }
    // An exact-scope filter (`--scope rjf.quotes`) yields one populated
    // scope: there is no hierarchy to reconstruct, so print it as a single
    // line with its full path instead of synthesizing a root per segment.
    if scopes.len() == 1 && top >= 1 {
        let s = &scopes[0];
        let last = s
            .last_activity_at
            .format(&time::format_description::well_known::Iso8601::DATE)
            .unwrap_or_else(|_| "?".into());
        return vec![
            format!("Scopes (1 populated){suffix}:"),
            format!("  {}  {}  last {last}", s.scope.as_str(), s.thought_count),
        ];
    }

    // 1. Trie over dotted segments (Scope validation guarantees non-empty
    //    segments, so there are no empty-string keys).
    let mut roots: std::collections::BTreeMap<String, ScopeTreeNode> =
        std::collections::BTreeMap::new();
    for s in scopes {
        let mut segs = s.scope.as_str().split('.');
        let first = segs.next().unwrap_or(s.scope.as_str());
        let mut node = roots.entry(first.to_string()).or_default();
        for seg in segs {
            node = node.children.entry(seg.to_string()).or_default();
        }
        node.own_count = Some(s.thought_count);
        node.own_last = Some(s.last_activity_at);
    }

    // 2. Rank populated scopes by subtree rollup desc (scope asc on ties),
    //    keep the top N.
    let mut ranked: Vec<(String, i64)> = Vec::new();
    for (seg, node) in &roots {
        collect_scope_paths(node, seg, &mut ranked);
    }
    let total_populated = ranked.len();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let truncated = total_populated > top;
    let kept: std::collections::HashSet<String> = if truncated {
        ranked.iter().take(top).map(|(s, _)| s.clone()).collect()
    } else {
        ranked.into_iter().map(|(s, _)| s).collect()
    };

    // 3. Depth-first render with box-drawing connectors, then a column pass.
    let mut lines: Vec<ScopeLine> = Vec::new();
    walk_scope_level(&roots, "", "  ", 0, &kept, &mut lines);

    let branch_only = lines
        .iter()
        .filter(|l| {
            matches!(
                l.kind,
                ScopeLineKind::Node {
                    branch_only: true,
                    ..
                }
            )
        })
        .count();
    // Absolute columns (char counts, not bytes — the connectors are
    // multibyte): prefix+name share one column, rollups right-align to a
    // single number column across the WHOLE tree (so `tech 296` and a
    // deeper leaf's `11` still line up), then the optional own-count note,
    // then the date.
    let name_col_w = lines
        .iter()
        .filter(|l| !matches!(l.kind, ScopeLineKind::Hidden { .. }))
        .map(|l| l.prefix.chars().count() + l.name.chars().count())
        .max()
        .unwrap_or(0);
    let notes: Vec<String> = lines
        .iter()
        .map(|l| match &l.kind {
            ScopeLineKind::Node { own: Some(o), .. } => format!(" ({o})"),
            _ => String::new(),
        })
        .collect();
    let num_w = lines
        .iter()
        .map(|l| match &l.kind {
            ScopeLineKind::Node { rollup, .. } => rollup.to_string().len(),
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    let note_w = notes.iter().map(|n| n.chars().count()).max().unwrap_or(0);

    let note = if truncated {
        format!(
            "  (showing top {top}, {} more hidden)",
            total_populated - top
        )
    } else {
        String::new()
    };
    let branch_note = if branch_only > 0 {
        format!(", {branch_only} branch-only")
    } else {
        String::new()
    };
    let mut out = Vec::with_capacity(lines.len() + 1);
    out.push(format!(
        "Scopes ({total_populated} populated{branch_note}){suffix}:{note}"
    ));
    for (line, note) in lines.iter().zip(notes) {
        match &line.kind {
            ScopeLineKind::Node { rollup, last, .. } => out.push(format!(
                "{:<name_col_w$}  {rollup:>num_w$}{note:<note_w$}  last {last}",
                format!("{}{}", line.prefix, line.name)
            )),
            ScopeLineKind::Hidden { scopes, thoughts } => out.push(format!(
                "{}… (+{scopes} scopes, {thoughts} thoughts)",
                line.prefix
            )),
        }
    }
    out
}

/// Render one level of the tree: visible children in alphabetical order
/// (BTreeMap iteration), then a single collapsed line for the pruned
/// remainder. `cont` is the ancestor continuation prefix; roots (depth 0)
/// take no connector.
fn walk_scope_level(
    children: &std::collections::BTreeMap<String, ScopeTreeNode>,
    path: &str,
    cont: &str,
    depth: usize,
    kept: &std::collections::HashSet<String>,
    lines: &mut Vec<ScopeLine>,
) {
    let shown: Vec<(&String, &ScopeTreeNode)> = children
        .iter()
        .filter(|(seg, c)| scope_node_shown(c, &scope_child_path(path, seg), kept))
        .collect();
    let hidden: Vec<&ScopeTreeNode> = children
        .iter()
        .filter(|(seg, c)| !scope_node_shown(c, &scope_child_path(path, seg), kept))
        .map(|(_, c)| c)
        .collect();
    let hidden_scopes = hidden.iter().map(|c| c.populated()).sum::<i64>();
    let hidden_thoughts = hidden.iter().map(|c| c.rollup()).sum::<i64>();

    for (i, (seg, node)) in shown.iter().enumerate() {
        let is_last = i + 1 == shown.len() && hidden_scopes == 0;
        let connector = if depth == 0 {
            ""
        } else if is_last {
            "└─ "
        } else {
            "├─ "
        };
        let own_smaller = match node.own_count {
            Some(o) if o < node.rollup() => Some(o),
            _ => None,
        };
        let last = node
            .last()
            .map(|t| {
                t.format(&time::format_description::well_known::Iso8601::DATE)
                    .unwrap_or_else(|_| "?".into())
            })
            .unwrap_or_else(|| "?".into());
        lines.push(ScopeLine {
            prefix: format!("{cont}{connector}"),
            name: (*seg).clone(),
            kind: ScopeLineKind::Node {
                rollup: node.rollup(),
                own: own_smaller,
                last,
                branch_only: node.own_count.is_none(),
            },
        });
        // Roots have no connector, so they contribute no continuation
        // slot; their children start at the root indent either way.
        let child_cont = if depth == 0 {
            cont.to_string()
        } else {
            format!("{cont}{}", if is_last { "   " } else { "│  " })
        };
        walk_scope_level(
            &node.children,
            &scope_child_path(path, seg),
            &child_cont,
            depth + 1,
            kept,
            lines,
        );
    }
    if hidden_scopes > 0 {
        let connector = if depth == 0 { "" } else { "└─ " };
        lines.push(ScopeLine {
            prefix: format!("{cont}{connector}"),
            name: "…".to_string(),
            kind: ScopeLineKind::Hidden {
                scopes: hidden_scopes,
                thoughts: hidden_thoughts,
            },
        });
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;

    fn scope_summary(scope: &str, count: i64, day: i64) -> kengram_storage::ScopeSummary {
        let at = time::OffsetDateTime::UNIX_EPOCH + time::Duration::days(day);
        kengram_storage::ScopeSummary {
            scope: kengram_core::Scope::new(scope).unwrap(),
            thought_count: count,
            first_activity_at: at,
            last_activity_at: at,
        }
    }

    #[test]
    fn humanize_bytes_renders_unit_scale() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(999), "999 B");
        assert_eq!(humanize_bytes(1024), "1 KB");
        assert_eq!(humanize_bytes(40 * 1024), "40 KB");
        assert_eq!(humanize_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(humanize_bytes(12 * 1024 * 1024), "12.0 MB");
        assert_eq!(humanize_bytes(1024_i64.pow(3)), "1.00 GB");
    }

    #[test]
    fn scopes_tree_alphabetizes_and_synthesizes_branch_nodes() {
        let lines = render_scopes_tree(
            &[
                scope_summary("zeroclaw", 26, 20),
                scope_summary("rjf.professional.cto", 17, 25),
                scope_summary("rjf.personal.fishing", 5, 9),
                scope_summary("rjf.ham-radio", 11, 13),
            ],
            20,
            None,
        );
        // rjf, personal, professional are branch-only: no thought sits there.
        assert_eq!(lines[0], "Scopes (4 populated, 3 branch-only):");
        let names: Vec<&str> = lines[1..]
            .iter()
            .map(|l| {
                l.trim_start_matches([' ', '│', '├', '└', '─'])
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(',')
            })
            .collect();
        // Alphabetical at every level; zeroclaw last among roots.
        assert_eq!(
            names,
            [
                "rjf",
                "ham-radio",
                "personal",
                "fishing",
                "professional",
                "cto",
                "zeroclaw"
            ]
        );
    }

    #[test]
    fn scopes_tree_shows_rollup_with_own_in_parens() {
        let lines = render_scopes_tree(
            &[
                scope_summary("rjf.tech", 280, 320),
                scope_summary("rjf.tech.squrl", 11, 100),
                scope_summary("rjf.tech.zed", 5, 120),
            ],
            20,
            None,
        );
        let tech = lines.iter().find(|l| l.contains("tech")).unwrap();
        assert!(tech.contains("296 (280)"), "rollup + own: {tech}");
        let squrl = lines.iter().find(|l| l.contains("squrl")).unwrap();
        assert!(squrl.contains(" 11 "), "leaf shows its own count: {squrl}");
        // Branch root: rollup only, no paren.
        let rjf = &lines[1];
        assert!(
            rjf.contains("296") && !rjf.contains("(296)"),
            "branch root: {rjf}"
        );
    }

    #[test]
    fn scopes_tree_connectors_and_continuation() {
        let lines = render_scopes_tree(
            &[
                scope_summary("rjf.a", 1, 1),
                scope_summary("rjf.a.x", 1, 2),
                scope_summary("rjf.b", 1, 3),
            ],
            20,
            None,
        );
        // Root at flat-list indent, no connector.
        assert!(lines[1].starts_with("  rjf "), "{}", lines[1]);
        assert!(lines[2].starts_with("  ├─ a "), "{}", lines[2]);
        // Grandchild under a NON-last parent gets the │ continuation.
        assert!(lines[3].starts_with("  │  └─ x "), "{}", lines[3]);
        // Last sibling gets └─.
        assert!(lines[4].starts_with("  └─ b "), "{}", lines[4]);
    }

    #[test]
    fn scopes_tree_truncates_by_rollup_and_collapses_pruned_siblings() {
        let lines = render_scopes_tree(
            &[
                scope_summary("rjf.tech", 100, 1),
                scope_summary("rjf.tech.squrl", 11, 2),
                scope_summary("small", 1, 3),
                scope_summary("tiny", 2, 4),
            ],
            2,
            None,
        );
        // Kept by rollup: rjf.tech (111) and squrl (11); small/tiny pruned.
        assert!(
            lines[0].contains("(showing top 2, 2 more hidden)"),
            "{}",
            lines[0]
        );
        let hidden = lines.iter().find(|l| l.contains('…')).unwrap();
        assert!(
            hidden.contains("(+2 scopes, 3 thoughts)"),
            "collapsed pruned roots: {hidden}"
        );
        assert!(lines.iter().any(|l| l.contains("111 (100)")));
    }

    #[test]
    fn scopes_tree_keeps_pruned_ancestors_as_structure() {
        let lines = render_scopes_tree(
            &[
                scope_summary("rjf.personal.fishing", 9, 1),
                scope_summary("other", 5, 2),
            ],
            1,
            None,
        );
        // rjf and personal have no thoughts of their own yet are shown so the
        // kept leaf keeps its parents; `other` collapses into the hidden line.
        assert!(lines.iter().any(|l| l.contains("personal")));
        assert!(lines.iter().any(|l| l.contains("(+1 scopes, 5 thoughts)")));
        assert!(lines[0].contains("2 branch-only"), "{}", lines[0]);
    }

    #[test]
    fn scopes_tree_single_scope_and_empty_edge_cases() {
        let one = render_scopes_tree(&[scope_summary("rjf.quotes", 1, 1)], 20, None);
        assert_eq!(one.len(), 2); // header + one line, no connectors
        assert!(one[1].starts_with("  rjf.quotes "), "{}", one[1]);

        let none = render_scopes_tree(&[], 20, Some("rjf."));
        assert_eq!(
            none,
            vec!["Scopes (0) matching prefix \"rjf.\": (no scopes)"]
        );
    }
}

#[cfg(test)]
mod tag_tests {
    use super::*;
    use kengram_core::ThoughtId;
    use kengram_storage::TagSnapshotRow;

    #[test]
    fn snapshot_rows_to_json_shapes_provenance_and_bare_uuid() {
        let id = ThoughtId::new();
        let rows = vec![
            TagSnapshotRow {
                thought_id: id,
                tags: serde_json::json!({"people": ["Ron"], "kind": "observation"}),
                tags_extractor_model: Some("ollama/test".to_string()),
                tags_extractor_version: Some(13),
            },
            TagSnapshotRow {
                thought_id: ThoughtId::new(),
                tags: serde_json::json!({}),
                tags_extractor_model: None,
                tags_extractor_version: None,
            },
        ];

        let json = snapshot_rows_to_json(&rows);

        assert_eq!(json.len(), 2);
        // thought_id is a bare UUID string, not a wrapper object.
        assert_eq!(
            json[0]["thought_id"].as_str().unwrap(),
            id.as_uuid().to_string()
        );
        assert_eq!(json[0]["tags"]["people"][0], "Ron");
        assert_eq!(json[0]["tags_extractor_model"], "ollama/test");
        assert_eq!(json[0]["tags_extractor_version"], 13);
        // Untagged row → null provenance.
        assert!(json[1]["tags_extractor_model"].is_null());
        assert!(json[1]["tags_extractor_version"].is_null());
    }
}

#[cfg(test)]
mod rescope_cli_tests {
    use super::{Cli, Command};
    use clap::Parser;

    fn parse(args: &[&str]) -> clap::error::Result<Cli> {
        Cli::try_parse_from(std::iter::once("kengram").chain(args.iter().copied()))
    }

    #[test]
    fn single_form_parses_with_defaults() {
        let cli = parse(&[
            "rescope",
            "--thought",
            "6f1c8a3e-0000-4000-8000-000000000000",
            "--to",
            "work.bar",
            "--reason",
            "taxonomy drift",
        ])
        .unwrap();
        match cli.command {
            Command::Rescope {
                thought,
                scope,
                to,
                reason,
                actor,
                dry_run,
            } => {
                assert!(thought.is_some());
                assert!(scope.is_none());
                assert_eq!(to, "work.bar");
                assert_eq!(reason, "taxonomy drift");
                assert_eq!(actor, "manual");
                assert!(!dry_run);
            }
            other => panic!("expected Rescope, got {other:?}"),
        }
    }

    #[test]
    fn subtree_form_with_dry_run_and_actor_parses() {
        let cli = parse(&[
            "rescope",
            "--scope",
            "work.foo",
            "--to",
            "work.bar",
            "--reason",
            "merge",
            "--actor",
            "ops:renaming",
            "--dry-run",
        ])
        .unwrap();
        match cli.command {
            Command::Rescope {
                thought,
                scope,
                dry_run,
                actor,
                ..
            } => {
                assert!(thought.is_none());
                assert_eq!(scope.as_deref(), Some("work.foo"));
                assert!(dry_run);
                assert_eq!(actor, "ops:renaming");
            }
            other => panic!("expected Rescope, got {other:?}"),
        }
    }

    #[test]
    fn reason_is_required() {
        let err = parse(&["rescope", "--thought", "x", "--to", "y"]);
        assert!(err.is_err(), "--reason missing must fail at parse time");
    }

    #[test]
    fn thought_and_scope_are_mutually_exclusive() {
        let err = parse(&[
            "rescope",
            "--thought",
            "x",
            "--scope",
            "y",
            "--to",
            "z",
            "--reason",
            "r",
        ]);
        assert!(err.is_err());
    }
}
