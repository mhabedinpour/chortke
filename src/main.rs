use chortke::{api, config};
use clap::{Parser, Subcommand};

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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let config = config::AppConfig::load(cli.config_path.as_ref()).expect("could not load config");

    init_logging(&config);

    match cli.command {
        Commands::Matcher => {
            api::start(&config.api)
                .await
                .expect("could not start API server");
        }
    }
}
