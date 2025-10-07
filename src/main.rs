use chortke::{api, config};
use clap::{Parser, Subcommand};
use tokio::signal::unix::signal;
use tokio::{select, signal};
use tokio_util::sync::CancellationToken;
use tracing::info;

#[derive(Parser)]
#[command(name = "chortke", about = "Chortke Trade Engine")]
struct Cli {
    #[arg(short, long, default_value = "config.toml")]
    config_path: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Matcher,
}

fn init_logging(cfg: &config::AppConfig) {
    match cfg.logger.format {
        config::LogFormat::JSON => {
            tracing_subscriber::fmt()
                .json()
                .with_max_level(cfg.logger.level)
                .with_current_span(true)
                .init();
        }
        config::LogFormat::COMPACT => {
            tracing_subscriber::fmt()
                .compact()
                .with_max_level(cfg.logger.level)
                .init();
        }
    }
}

pub fn setup_shutdown_token() -> CancellationToken {
    let token = CancellationToken::new();
    let trigger = token.clone();

    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();
        let mut terminate = signal(signal::unix::SignalKind::terminate()).unwrap();

        select! {
            _ = ctrl_c => { },
            _ = terminate.recv() => {  }
        }

        info!("shutdown signal received, exiting");
        trigger.cancel();
    });

    token
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let config = config::AppConfig::load(cli.config_path.as_ref()).expect("could not load config");

    init_logging(&config);

    let shutdown_token = setup_shutdown_token();

    match cli.command {
        Commands::Matcher => {
            api::start(&config.api, shutdown_token.clone())
                .await
                .expect("could not start API server");
        }
    }
}
