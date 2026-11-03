use clap::Parser;

/// Lumen: animated wallpaper engine.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Wallpaper folder, project.json path, or Workshop ID.
    background: Option<String>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    tracing::info!(?cli, "starting");
    Ok(())
}
