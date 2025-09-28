import os
from typing import Optional

from .utils import which, run, info, die
from .config import PLAYER_PREF

class Player:
    def __init__(self, prefer: Optional[str] = None):
        self.player = None
        self.bin = None
        self._select_player(prefer)

    def _select_player(self, prefer: Optional[str]):
        search = []
        if prefer:
            search.append(prefer)
        search.extend([p for p in PLAYER_PREF if p != prefer])
        for p in search:
            b = which(p)
            if b:
                self.player = p
                self.bin = b
                info(f"using player: {p}")
                return
        die(
            "no supported audio player found. install mpv or ffmpeg, or use macOS (afplay).\n"
            "  - brew install mpv\n  - or: brew install ffmpeg"
        )

    def play(self, filepath: str, volume: Optional[float] = None):
        if not os.path.exists(filepath):
            die(f"file not found: {filepath}")
        cmd = [self.bin]
        p = self.player
        if p == "afplay":
            if volume is not None:
                cmd += ["-v", str(max(0.0, min(1.0, volume)))]
            cmd += [filepath]
        elif p == "mpv":
            if volume is not None:
                v = int(max(0, min(100, round(volume * 100))))
                cmd += ["--volume", str(v)]
            cmd += ["--no-video", "--", filepath]
        elif p == "ffplay":
            cmd += ["-nodisp", "-autoexit", "-loglevel", "warning", filepath]
        else:
            die(f"unknown player: {p}")
        try:
            run(cmd, check=True)
        except KeyboardInterrupt:
            info("stopped by user")
