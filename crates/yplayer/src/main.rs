use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;
use yplayer::config::{self, Config};
use yplayer::download::bridge::Bridge;
use yplayer::service::{self, ServeOptions};
use yplayer::types::Track;

#[derive(Parser, Debug)]
#[command(name = "yplay", about = "Fast YouTube audio player with local cache")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Search YouTube
    Search {
        query: String,
        /// Maximum number of results
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// List audio formats for a URL
    Formats { url: String },
    /// Run the background service
    Serve {
        /// Cache directory (default: config file, else ~/Music/yt-audio)
        #[arg(long)]
        dir: Option<String>,
        /// Socket path (default: $YPLAY_SOCKET, else <state dir>/yplay.sock)
        #[arg(long)]
        socket: Option<PathBuf>,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        return Ok(());
    };

    let file = config::FileConfig::load();
    let dir = match &command {
        Commands::Serve { dir, .. } => dir.clone(),
        _ => None,
    };
    let mut cfg = Config::new(dir.or(file.cache_dir), file.api_key);
    cfg.volume = file.volume;
    cfg.worker_python = file.worker_python;

    // Ensure cache directory exists
    std::fs::create_dir_all(&cfg.cache_dir)?;

    match command {
        Commands::Search { query, limit } => {
            let mut bridge = Bridge::new(&cfg).await?;
            let results = bridge.search(&query, limit, &cfg).await?;
            print_search_results(&results);
        }
        Commands::Formats { url } => {
            let mut bridge = Bridge::new(&cfg).await?;
            let formats = bridge.list_formats(&url).await?;
            for f in formats {
                println!("{}", f);
            }
        }
        Commands::Serve { socket, .. } => {
            service::serve(ServeOptions {
                config: cfg,
                socket_path: socket.unwrap_or_else(Config::socket_path),
                state_dir: Config::state_dir(),
            })
            .await?;
        }
    }

    Ok(())
}

fn print_search_results(results: &[Track]) {
    for (i, r) in results.iter().enumerate() {
        let title = &r.title;
        let uploader = r.uploader.as_deref().unwrap_or("?");
        let dur = format_duration(r.duration);
        let url = r.webpage_url.as_deref().unwrap_or("");
        println!("  \x1b[35m[{:02}]\x1b[0m \x1b[34m{}\x1b[0m", i + 1, title);
        println!(
            "    \x1b[33m{}\x1b[0m \u{2022} \x1b[2m{}\x1b[0m",
            uploader, dur
        );
        println!("    \x1b[92m{}\x1b[0m\n", url);
    }
}

fn format_duration(sec: Option<i64>) -> String {
    match sec {
        Some(s) => {
            let h = s / 3600;
            let m = (s % 3600) / 60;
            let sec = s % 60;
            if h > 0 {
                format!("{}:{:02}:{:02}", h, m, sec)
            } else {
                format!("{}:{:02}", m, sec)
            }
        }
        None => "?:??".to_string(),
    }
}
