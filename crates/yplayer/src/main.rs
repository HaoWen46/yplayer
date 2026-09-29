use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;
use yplayer::client::{self, AddOptions};
use yplayer::config::{self, Config};
use yplayer::download::bridge::Bridge;
use yplayer::protocol::Command;
use yplayer::service::{self, ServeOptions};
use yplayer::types::Track;

#[derive(Parser, Debug)]
#[command(
    name = "yplay",
    version,
    about = "Fast YouTube audio player with local cache"
)]
struct Cli {
    /// Service socket (default: $YPLAY_SOCKET, else <state dir>/yplay.sock)
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the background service
    Serve {
        /// Cache directory (default: config file, else ~/Music/yt-audio)
        #[arg(long)]
        dir: Option<String>,
    },
    /// Add a YouTube URL to an album and play it
    Add {
        url: String,
        /// Album name (default: the last used album, else Inbox)
        #[arg(long)]
        album: Option<String>,
        /// Do not start playback
        #[arg(long)]
        no_play: bool,
        /// Wait for the download and report timings
        #[arg(long)]
        wait: bool,
    },
    /// Play a track from the library or an album
    Play {
        track_id: String,
        /// Album name to play from (default: the whole library)
        #[arg(long)]
        album: Option<String>,
    },
    /// Pause playback
    Pause,
    /// Resume playback
    Resume,
    /// Toggle pause
    Toggle,
    /// Stop playback
    Stop,
    /// Next track
    Next,
    /// Previous track
    Prev,
    /// Show the current track
    Now,
    /// List albums
    Albums,
    /// Search YouTube
    Search {
        query: String,
        /// Maximum number of results
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// List audio formats for a URL
    Formats { url: String },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        return Ok(());
    };
    let socket = cli.socket.unwrap_or_else(Config::socket_path);

    let result = match command {
        Commands::Serve { dir } => {
            let cfg = load_config(dir)?;
            return service::serve(ServeOptions {
                config: cfg,
                socket_path: socket,
                state_dir: Config::state_dir(),
            })
            .await;
        }
        Commands::Search { query, limit } => {
            let cfg = load_config(None)?;
            let mut bridge = Bridge::new(&cfg).await?;
            let results = bridge.search(&query, limit, &cfg).await?;
            print_search_results(&results);
            return Ok(());
        }
        Commands::Formats { url } => {
            let cfg = load_config(None)?;
            let mut bridge = Bridge::new(&cfg).await?;
            let formats = bridge.list_formats(&url).await?;
            for f in formats {
                println!("{}", f);
            }
            return Ok(());
        }
        Commands::Add {
            url,
            album,
            no_play,
            wait,
        } => {
            client::add(
                &socket,
                AddOptions {
                    url,
                    album,
                    play: !no_play,
                    wait,
                },
            )
            .await
        }
        Commands::Play { track_id, album } => client::play(&socket, track_id, album).await,
        Commands::Pause => client::send(&socket, Command::Pause).await,
        Commands::Resume => client::send(&socket, Command::Resume).await,
        Commands::Toggle => client::send(&socket, Command::Toggle).await,
        Commands::Stop => client::send(&socket, Command::Stop).await,
        Commands::Next => client::send(&socket, Command::Next).await,
        Commands::Prev => client::send(&socket, Command::Prev).await,
        Commands::Now => client::now(&socket).await,
        Commands::Albums => client::albums(&socket).await,
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(e.exit_code());
    }
    Ok(())
}

/// Config file values, with `dir` overriding the cache directory (created).
fn load_config(dir: Option<String>) -> anyhow::Result<Config> {
    let file = config::FileConfig::load();
    let mut cfg = Config::new(dir.or(file.cache_dir), file.api_key);
    cfg.volume = file.volume;
    cfg.worker_python = file.worker_python;

    // Ensure cache directory exists
    std::fs::create_dir_all(&cfg.cache_dir)?;
    Ok(cfg)
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
