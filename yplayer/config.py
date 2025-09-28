import os

# Defaults
DEFAULT_CACHE_DIR = os.path.expanduser("~/Music/yt-audio")
DEFAULT_AUDIO_FORMAT = "mp3"
SUPPORTED_FORMATS = ["mp3", "m4a", "opus", "flac", "wav"]
EMBED_METADATA_DEFAULT = True

# Preferred local players
PLAYER_PREF = ["afplay", "mpv", "ffplay"]

# Known possible output extensions
KNOWN_EXTS = [
    "mp3", "m4a", "opus", "flac", "wav", "webm", "ogg", "oga", "aac"
]

# Kept for compatibility; we select bestaudio via library opts
YTDL_AUDIO_FORMAT = "bestaudio/best"
