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
    let manager = AgentManager::new();
    let router = app(manager);

    tracing::info!("agent-gateway listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind TCP listener");
    axum::serve(listener, router)
        .await
        .expect("server error");
}
