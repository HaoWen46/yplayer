use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub cache_dir: PathBuf,
    pub api_key: Option<String>,
    pub format: String,
    pub native: bool,
    pub embed_meta: bool,
    pub audio_quality: Option<String>,
    pub player: Option<String>,
    pub volume: Option<f64>,
    pub prefetch_count: usize,
}

impl Config {
    pub fn new(cache_dir: Option<String>, api_key: Option<String>) -> Self {
        let cache_dir = cache_dir.map(PathBuf::from).unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Music")
                .join("yt-audio")
        });

        let api_key = api_key.or_else(|| std::env::var("YT_API_KEY").ok());

        Self {
            cache_dir,
            api_key,
            format: "mp3".to_string(),
            native: false,
            embed_meta: true,
            audio_quality: None,
            player: None,
            volume: None,
            prefetch_count: 3,
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.cache_dir.join(".yplayer.db")
    }
}
