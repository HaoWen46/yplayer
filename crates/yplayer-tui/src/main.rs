mod app;
mod cache;
mod config;
mod download;
mod events;
mod player;
mod types;
mod ui;

use clap::Parser;
use config::Config;

#[derive(Parser, Debug)]
#[command(name = "yplay", about = "Fast YouTube audio player with local cache")]
struct Cli {
    /// YouTube URL or search query
    query: Option<String>,

    /// Cache directory
    #[arg(long = "dir")]
    dir: Option<String>,

    /// Download to cache but do not play
    #[arg(long = "download-only")]
    download_only: bool,

    /// Audio format (mp3, m4a, opus, flac, wav)
    #[arg(long, default_value = "mp3")]
    format: String,

    /// Skip conversion and metadata embedding
    #[arg(long)]
    native: bool,

    /// Disable embedding metadata
    #[arg(long = "no-meta")]
    no_meta: bool,

    /// ffmpeg audio quality hint
    #[arg(long = "audio-quality")]
    audio_quality: Option<String>,

    /// Preferred player binary (mpv/ffplay/afplay)
    #[arg(long)]
    player: Option<String>,

    /// Volume 0.0-1.0
    #[arg(long)]
    volume: Option<f64>,

    /// Browse local library
    #[arg(long)]
    browse: bool,

    /// List audio formats for a URL and exit
    #[arg(long = "list-formats")]
    list_formats: bool,

    /// YouTube Data API key
    #[arg(long = "yt-api-key")]
    yt_api_key: Option<String>,

    /// How many tracks to prefetch for playlists
    #[arg(long, default_value = "3")]
    prefetch: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let mut cfg = Config::new(cli.dir, cli.yt_api_key);
    cfg.format = cli.format;
    cfg.native = cli.native;
    cfg.embed_meta = !cli.no_meta;
    cfg.audio_quality = cli.audio_quality;
    cfg.player = cli.player;
    cfg.volume = cli.volume;
    cfg.prefetch_count = cli.prefetch;

    // Ensure cache directory exists
    std::fs::create_dir_all(&cfg.cache_dir)?;

    if cli.browse || (cli.query.is_none() && !cli.list_formats) {
        // Launch TUI
        app::run(cfg).await?;
    } else if let Some(query) = cli.query {
        if cli.list_formats {
            // List formats via Python worker
            let mut bridge = download::bridge::Bridge::new(&cfg).await?;
            let formats = bridge.list_formats(&query).await?;
            for f in formats {
                println!("{}", f);
            }
        } else if is_url(&query) {
            // Download (and optionally play)
            let mut bridge = download::bridge::Bridge::new(&cfg).await?;
            let result = bridge.download(&query, &cfg).await?;
            println!("{}", result.path);

            if !cli.download_only {
                let mut mpv = player::mpv::MpvPlayer::new();
                mpv.play(&result.path, cfg.volume).await?;
                // Wait for playback to finish
                mpv.wait_until_done().await;
            }
        } else {
            // Search query
            let mut bridge = download::bridge::Bridge::new(&cfg).await?;
            let results = bridge.search(&query, 10, &cfg).await?;
            print_search_results(&results);
        }
    }

    Ok(())
}

fn is_url(s: &str) -> bool {
    s.contains("youtube.com") || s.contains("youtu.be")
}

fn print_search_results(results: &[types::Track]) {
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
