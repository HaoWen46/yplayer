# yplayer/cli.py
import argparse
import os
import sys

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
            "Download/play YouTube audio with a local cache.\n\n"
            "Common usage:\n"
            "  yplay \"some artist\"           # search (prints top results)\n"
            "  yplay https://youtu.be/VIDEOID  # play or download single video\n"
            "  yplay --browse                  # interactive browse of cached library\n"
            "  yplay --prefetch 5 <playlist>   # browse a playlist and prefetch ahead\n"
        ),
        formatter_class=argparse.RawTextHelpFormatter,
    )
    p.add_argument("query", nargs="?", help="YouTube URL or a search query")
    p.add_argument("--dir", default=DEFAULT_CACHE_DIR, help="cache directory")
    p.add_argument("--download-only", action="store_true", help="download to cache but do not play")
    p.add_argument("--format", default="mp3", choices=SUPPORTED_FORMATS, help="audio format")
    p.add_argument("--native", action="store_true", help="skip conversion and metadata embedding")
    p.add_argument("--no-meta", action="store_true", help="disable embedding metadata")
    p.add_argument("--audio-quality", default=None, help="ffmpeg audio quality hint")
    p.add_argument("--player", default=None, help="preferred player binary (mpv/ffplay/afplay)")
    p.add_argument("--volume", type=float, default=None, help="volume 0.0–1.0 (mpv only)")
    p.add_argument("--browse", action="store_true", help="browse local library")
    p.add_argument("--list-formats", action="store_true", help="list audio formats for a URL and exit")
    p.add_argument("--yt-api-key", default=os.environ.get("YT_API_KEY"), help="YouTube Data API key")
    p.add_argument("--prefetch", type=int, default=3, help="how many tracks to prefetch for playlists (default: 3)")
    p.add_argument("--prefetch-count", type=int, default=None, help=argparse.SUPPRESS)
    return p


def _fmt_dur(sec):
    if sec is None:
        return "?:??"
    try:
        sec = int(sec)
    except Exception:
        return "?:??"
    m, s = divmod(sec, 60)
    h, m = divmod(m, 60)
    return f"{h:d}:{m:02d}:{s:02d}" if h else f"{m:d}:{s:02d}"


def _print_search_results(results):
    for i, r in enumerate(results, start=1):
        title = r.get("title") or r.get("id")
        url = r.get("webpage_url") or ""
        uploader = r.get("uploader") or "?"
        dur = _fmt_dur(r.get("duration"))
        print(f"  {Colors.MAGENTA}[{i:02d}] {Colors.BLUE}{title}{Colors.RESET}")
        print(f"    {Colors.YELLOW}{uploader}{Colors.RESET} • {Colors.DIM}{dur}{Colors.RESET}")
        print(f"    {Colors.BRIGHT_GREEN}{url}{Colors.RESET}\n")


def main(argv=None):
    if argv is None:
        argv = sys.argv[1:]
    parser = _mk_parser()
    args = parser.parse_args(argv)

    # Make --prefetch-count alias backward-compatible if used
    if args.prefetch_count is not None:
        prefetch = args.prefetch_count
    else:
        prefetch = args.prefetch

    # If no arguments and not browse, show helpful usage + examples
    if not argv or (len(argv) == 1 and args.query is None and not args.browse):
        parser.print_help()
        print("\nExamples:")
        print("  yplay \"zutomayo\"")
        print("  yplay https://youtu.be/abc123xyz")
        print("  yplay --browse")
        print("  yplay --prefetch 5 \"https://www.youtube.com/playlist?list=PL...\"")
        return

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
    opts.print_only = bool(args.download_only)
    if args.download_only:
        opts.play_after = False


    if args.browse:
        tracks = list_cached_tracks(args.dir)
        browse_and_play(tracks, prefer_player=args.player, volume=args.volume)
        return

    if not args.query:
        # This branch is mostly unreachable because we handle no-argv above,
        # but keep for safety.
        parser.print_help()
        return

    # If it's a URL, treat playlist specially
    if is_url(args.query):
        if is_playlist_url(args.query):
            entries = extract_playlist_entries(args.query)
            if not entries:
                info("playlist appears empty")
                return
            browse_playlist(
                entries,
                opts,
                prefetch_count=prefetch,
                api_key=args.yt_api_key,
                prefer_player=args.player,
                volume=args.volume,
            )
            return

        # Single video URL path
        if args.list_formats:
            fmts = list_audio_formats(args.query)
            for f in fmts:
                print(f)
            return
        run_and_maybe_play(args.query, opts, api_key=args.yt_api_key)
        return

    # Otherwise treat as a search query
    try:
        results = search_results(args.query, 10, api_key=args.yt_api_key)
    except Exception as e:
        info(f"search failed: {e}")
        return

    if not results:
        info("no results")
        return

    _print_search_results(results)


if __name__ == "__main__":
    main()
