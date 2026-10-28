use std::sync::Arc;

use gateway::{
    AppState, app,
    budget::Ledger,
    config::{Config, ProviderKind},
    provider::{
        Provider, anthropic::AnthropicProvider, gemini::GeminiProvider, mock::MockProvider,
        openai::OpenAiProvider,
    },
    tokens::TokenStore,
    vault::Vault,
};
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let pool = gateway::db::connect(&config.database_url).await?;
    tracing::info!("database ready, migrations applied");

    let vault = Arc::new(Vault::new(pool.clone(), &config.master_key)?);
    let ledger = Arc::new(Ledger::new(pool.clone(), config.monthly_budget_micros));
    ledger.spawn_stale_reservation_sweeper();

    let provider: Arc<dyn Provider> = match config.provider {
        ProviderKind::Mock => {
            tracing::warn!("using the mock provider: no real model is called");
            Arc::new(MockProvider::new())
        }
        ProviderKind::OpenAi => {
            tracing::info!("using the OpenAI provider");
            Arc::new(OpenAiProvider::new(Arc::clone(&vault))?)
        }
        ProviderKind::Anthropic => {
            tracing::info!("using the Anthropic provider");
            Arc::new(AnthropicProvider::new(Arc::clone(&vault))?)
        }
        ProviderKind::Gemini => {
            let [small, standard, large] = config.gemini_models.ids();
            tracing::info!(small, standard, large, "using the Gemini provider");
            Arc::new(GeminiProvider::new(
                Arc::clone(&vault),
                config.gemini_models,
            )?)
        }
    };

    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(bind = %config.bind, "gateway listening");

    let state = AppState::new(
        provider,
        Arc::new(TokenStore::new(pool.clone())),
        ledger,
        vault,
        &config.issuer_secret,
        &config.admin_secret,
    );
    drop(config);

    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("gateway stopped");
    Ok(())
}

/// Resolves on Ctrl+C or SIGTERM so in-flight requests can finish.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
