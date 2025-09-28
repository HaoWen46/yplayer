# yplayer/enhanced_playback.py
import os
import subprocess
from typing import Optional
from .utils import which, info
from .mpv_player import MPVPlayer

class EnhancedPlayer:
    def __init__(self, prefer: Optional[str] = None):
        self.mpv_player = None
        self.use_mpv = False
        self._select_player(prefer)

    def _select_player(self, prefer: Optional[str]):
        # Prioritize mpv for proper pause/resume support
        if which("mpv"):
            self.mpv_player = MPVPlayer()
            self.use_mpv = True
            info("using player: mpv (with pause/resume support)")
        else:
            # Fallback to simple players (no pause support)
            search = ["afplay", "ffplay"]
            if prefer:
                search = [prefer] + [p for p in search if p != prefer]

            for p in search:
                b = which(p)
                if b:
                    self.player_bin = b
                    self.player_name = p
                    info(f"using player: {p} (no pause support)")
                    return

            raise Exception(
                "no supported audio player found. Install mpv for best experience:\n"
                "  - brew install mpv\n"
                "  - or: sudo apt install mpv"
            )

    def supports_pause(self):
        return self.use_mpv

    def play(self, filepath: str, volume: Optional[float] = None):
        """Play a file - stops any existing playback first"""
        if self.use_mpv:
            return self.mpv_player.play(filepath, volume)
        else:
            # Stop existing playback
            self.stop()

            if not os.path.exists(filepath):
                return False

            cmd = [self.player_bin]
            if self.player_name == "afplay":
                cmd += [filepath]
                if volume is not None:
                    cmd += ["-v", str(max(0.0, min(1.0, volume)))]
            elif self.player_name == "ffplay":
                cmd += ["-nodisp", "-autoexit", "-loglevel", "warning", filepath]

            try:
                self.process = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                self._is_playing = True
                return True
            except Exception:
                self._is_playing = False
                return False

    def pause_resume(self):
        if self.use_mpv:
            return self.mpv_player.pause_resume()
        return False

    def stop(self):
        if self.use_mpv:
            self.mpv_player.stop()
        else:
            if hasattr(self, 'process') and self.process:
                try:
                    self.process.terminate()
                    self.process.wait(timeout=2)
                except Exception:
                    try:
                        self.process.kill()
                    except Exception:
                        pass
                finally:
                    self.process = None
                    self._is_playing = False

    def is_playing(self):
        if self.use_mpv:
            return self.mpv_player.is_playing()
        else:
            if not hasattr(self, 'process') or not self.process:
                return False
            if self.process.poll() is not None:
                self._is_playing = False
                return False
            return getattr(self, '_is_playing', False)

    def is_paused(self):
        if self.use_mpv:
            return self.mpv_player.is_paused()
        return False
