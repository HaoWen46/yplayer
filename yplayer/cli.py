import argparse
import os

from .core import (
    Options,
    run_and_maybe_play,
    list_audio_formats,
    is_url,
    search_results,
    list_cached_tracks,
)
from .browse import browse_and_play, browse_playlist
from .config import DEFAULT_CACHE_DIR, SUPPORTED_FORMATS
from .utils import info, Colors
from .playlist import is_playlist_url, extract_playlist_entries


def _mk_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="yplay",
        description=(
            "Download/play YouTube audio with a local cache. "
            "Passing a non-URL will SHOW TOP RESULTS and EXIT (no download). "
            "Use --browse to pick from your local library."
        ),
    )
    p.add_argument("query", nargs="?", help="YouTube URL or a search query")
    p.add_argument("--dir", default=DEFAULT_CACHE_DIR, help="cache directory")
    p.add_argument("--format", default="mp3", choices=SUPPORTED_FORMATS, help="audio format")
    p.add_argument("--native", action="store_true", help="skip conversion and metadata embedding")
    p.add_argument("--no-meta", action="store_true", help="disable embedding metadata")
    p.add_argument("--audio-quality", default=None, help="ffmpeg audio quality hint")
    p.add_argument("--player", default=None, help="preferred player binary (afplay/mpv/ffplay)")
    p.add_argument("--volume", type=float, default=None, help="volume 0.0–1.0 (mpv only)")
    p.add_argument("--browse", action="store_true", help="browse local library")
    p.add_argument("--list-formats", action="store_true", help="list audio formats for a URL and exit")
    p.add_argument("--yt-api-key", default=os.environ.get("YT_API_KEY"), help="YouTube Data API key")
    p.add_argument("--prefetch", type=int, default=3, help="how many tracks to prefetch for playlists (default: 3)")
    return p


def _fmt_dur(sec):
    if sec is None:
        return "?:??"
    sec = int(sec)
    m, s = divmod(sec, 60)
    h, m = divmod(m, 60)
    return f"{h:d}:{m:02d}:{s:02d}" if h else f"{m:d}:{s:02d}"


def main():
    args = _mk_parser().parse_args()

    # construct Options for core
    opts = Options()
    opts.cache_dir = args.dir
    opts.fmt = args.format
    opts.native = bool(args.native)
    opts.embed_meta = not bool(args.no_meta)
    opts.audio_quality = args.audio_quality
    opts.player = args.player
    opts.play_after = True
    opts.volume = args.volume
    opts.print_only = False

    if args.browse:
        tracks = list_cached_tracks(args.dir)
        browse_and_play(tracks, prefer_player=args.player, volume=args.volume)
        return

    if not args.query:
        print("nothing to do — pass a URL or a search query (or --browse).")
        return

    if is_url(args.query):
        # playlist?
        if is_playlist_url(args.query):
            entries = extract_playlist_entries(args.query)
            if not entries:
                print("playlist appears empty")
                return
            browse_playlist(
                entries,
                opts,
                prefetch_count=args.prefetch,
                api_key=args.yt_api_key,
                prefer_player=args.player,
                volume=args.volume,
            )
            return

        # single video URL
        if args.list_formats:
            fmts = list_audio_formats(args.query)
            print("\n".join(fmts))
            return
        run_and_maybe_play(args.query, opts, api_key=args.yt_api_key)
        return

    # search (top 10)
    results = search_results(args.query, 10, api_key=args.yt_api_key)
    if not results:
        info("no results")
        return

    for i, r in enumerate(results, start=1):
        title = r.get("title") or r.get("id")
        url = r.get("webpage_url") or ""
        uploader = r.get("uploader") or "?"
        dur = _fmt_dur(r.get("duration"))
        print(f"  {Colors.MAGENTA}[{i:02d}] {Colors.BLUE}{title}{Colors.RESET}")
        print(f"    {Colors.YELLOW}{uploader}{Colors.RESET} • {Colors.DIM}{dur}{Colors.RESET}")
        print(f"    {Colors.BRIGHT_GREEN}{url}{Colors.RESET}\n")


if __name__ == "__main__":
    main()
