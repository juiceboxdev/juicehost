//! juicehost: the file storage side of juicebox.
//! files get pushed here by juiceback, stored on disk (or S3), served with
//! ETags, and cleaned up when they expire. also has an optional QUIC/HTTP/3 port
//! over QUIC/HTTP/3.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::{fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt};

use juicehost::config::Config;
use juicehost::server::{build_router, print_startup_banner, start_server};
use juicehost::state::AppState;
use juicehost::storage::{LocalBackend, S3Backend, StorageBackend};
#[cfg(feature = "quic")]
use tokio::sync::Notify;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let _ = dotenvy::dotenv();

    // Initialize Sentry before configuration so it captures startup failures.
    let _sentry_guard = std::env::var("SENTRY_DSN_JUICEHOST")
        .or_else(|_| std::env::var("SENTRY_DSN"))
        .ok()
        .filter(|dsn| !dsn.is_empty())
        .map(|dsn| {
            sentry::init((
                dsn.as_str(),
                sentry::ClientOptions {
                    release: sentry::release_name!(),
                    environment: Some(
                        std::env::var("SENTRY_ENVIRONMENT")
                            .unwrap_or_else(|_| "production".into())
                            .into(),
                    ),
                    traces_sample_rate: std::env::var("SENTRY_TRACES_SAMPLE_RATE")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0.05),
                    send_default_pii: false,
                    ..Default::default()
                },
            ))
        });

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer().with_span_events(FmtSpan::CLOSE))
        .with(sentry_tracing::layer())
        .init();

    let config = Config::from_env().expect("Failed to load configuration");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.worker_threads)
        .enable_all()
        .build()
        .expect("tokio runtime creation failed");

    runtime.block_on(async {
        // Create the appropriate storage backend
        let storage: Arc<dyn StorageBackend> = if config.is_s3_mode() {
            let bucket = config.s3_bucket.as_deref().unwrap();
            let region = config.s3_region.as_deref().unwrap_or("us-east-1");
            let endpoint = config.s3_endpoint.as_deref();
            let access_key = config.s3_access_key.as_deref().unwrap_or("");
            let secret_key = config.s3_secret_key.as_deref().unwrap_or("");

            tracing::info!("S3 storage mode: bucket={}, region={}", bucket, region);

            let backend = S3Backend::new(
                bucket,
                region,
                endpoint,
                access_key,
                secret_key,
                config.s3_allow_http,
            )
                .expect("Failed to create S3 storage backend");
            Arc::new(backend)
        } else {
            if !config.files_dir.exists() {
                std::fs::create_dir_all(&config.files_dir)
                    .expect("failed to create files directory");
                tracing::info!("created files directory at: {:?}", config.files_dir);
            }

            let backend = LocalBackend::new(config.files_dir.clone(), config.min_free_space_bytes)
                .expect("invalid files directory");
            backend.init_cache().await.expect("invalid files directory entry");
            Arc::new(backend)
        };

        let state = Arc::new(AppState::new(&config, storage));

        if !config.ip_pepper.is_empty()
            || config.ban_list_file.is_some()
            || config.ban_sync_url.is_some()
        {
            tokio::spawn(juicehost::ban::ban_refresh_loop(Arc::clone(&state)));
        }

        let app = build_router(Arc::clone(&state));

        let addr = SocketAddr::from((
            config
                .public_host
                .parse::<std::net::IpAddr>()
                .expect("Invalid PUBLIC_HOST"),
            config.public_port,
        ));

        #[cfg(feature = "quic")]
        {
            let shutdown = Arc::new(Notify::new());
            let quic_shutdown = Arc::clone(&shutdown);

            let quic_addr = SocketAddr::from((
                config
                    .quic_host
                    .parse::<std::net::IpAddr>()
                    .expect("Invalid QUIC_HOST"),
                config.quic_port,
            ));

            let quic_router = build_router(Arc::clone(&state));
            let quic_cert = config.quic_cert_path.clone();

            tokio::select! {
                _ = async {
                    let quic_limits = juiceutils::QuicServerLimits {
                        max_connections: config.quic_max_connections,
                        max_requests: config.quic_max_requests,
                        handshake_timeout: Duration::from_secs(config.quic_handshake_seconds),
                        idle_timeout: Duration::from_secs(config.quic_idle_seconds),
                        request_timeout: Duration::from_secs(config.quic_request_total_seconds),
                    };
                    tokio::spawn(juiceutils::start_quic_server_with_limits(quic_router, quic_addr, quic_shutdown, "juicehost", quic_cert, quic_limits));
                    print_startup_banner(&config);
                    start_server(app, addr, config.tcp_max_concurrent_requests).await;
                } => {}
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("shutting down...");
                    shutdown.notify_one();
                }
            }
        }

        #[cfg(not(feature = "quic"))]
        {
            print_startup_banner(&config);
            start_server(app, addr, config.tcp_max_concurrent_requests).await;
        }
    });

    // Flush any remaining Sentry events before exit
    if let Some(client) = sentry::Hub::current().client() {
        client.close(Some(Duration::from_secs(2)));
    }
}
