use agent_gateway::{app, AgentManager, CliOutcome, ServerConfig};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    let config = match ServerConfig::from_args_and_env(std::env::args(), |k| std::env::var(k).ok()) {
        Ok(CliOutcome::Run(cfg)) => cfg,
        Ok(CliOutcome::Help(help)) => {
            print!("{help}");
            return;
        }
        Ok(CliOutcome::Version(ver)) => {
            println!("{ver}");
            return;
        }
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(2);
        }
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agent_gateway=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let addr = config.socket_addr();
    let default_agent = config.default_agent.clone();
    let token_was_configured = config.token.is_some();
    let manager = AgentManager::with_config(&config);
    let active_token = manager.token().to_string();

    if let Some(token_path) = ServerConfig::default_token_path(&|k| std::env::var(k).ok()) {
        match ServerConfig::write_token_file(&token_path, &active_token) {
            Ok(()) => {
                if token_was_configured {
                    tracing::info!(
                        "auth token loaded from config/env/CLI and written to {}",
                        token_path.display()
                    );
                } else {
                    tracing::info!(
                        "generated random auth token: {} (saved to {})",
                        active_token,
                        token_path.display()
                    );
                }
            }
            Err(err) => {
                tracing::warn!("{err}");
                if !token_was_configured {
                    tracing::info!("generated random auth token: {}", active_token);
                }
            }
        }
    } else if !token_was_configured {
        tracing::info!("generated random auth token: {}", active_token);
    }

    let router = app(manager);

    tracing::info!(
        "agent-gateway listening on http://{} (default agent: {})",
        addr,
        default_agent
    );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind TCP listener");
    axum::serve(listener, router)
        .await
        .expect("server error");
}
