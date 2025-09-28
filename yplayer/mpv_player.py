# yplayer/mpv_player.py
import os
import subprocess
import tempfile
import time
import json
from typing import Optional

class MPVPlayer:
    def __init__(self):
        self.process = None
        self.socket_path = None
        self._is_playing = False
        self._is_paused = False
        self._current_file = None

    def _get_socket_path(self):
        """Create a unique socket path"""
        if not self.socket_path:
            self.socket_path = os.path.join(tempfile.gettempdir(), f"yplayer_mpv_{os.getpid()}")
        return self.socket_path

    def play(self, filepath: str, volume: Optional[float] = None):
        """Play with mpv using IPC socket"""
        if not os.path.exists(filepath):
            return False

        # Stop any existing playback first
        self.stop()

        socket_path = self._get_socket_path()
        cmd = ["mpv", "--no-video", "--idle=no", "--keep-open=no",
               f"--input-ipc-server={socket_path}", "--", filepath]

        if volume is not None:
            v = int(max(0, min(100, round(volume * 100))))
            cmd.insert(1, f"--volume={v}")

        try:
            # Use preexec_fn to create new process group for clean termination
            import signal
            def preexec_fn():
                os.setsid()
            self.process = subprocess.Popen(
                cmd,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                preexec_fn=preexec_fn
            )
            self._current_file = filepath
            self._is_playing = True
            self._is_paused = False

            # Wait a moment for socket to be ready
            time.sleep(0.1)
            return True

        except Exception as e:
            print(f"MPV playback error: {e}")
            self._is_playing = False
            return False

    def _send_command(self, command: list) -> bool:
        """Send command to mpv via socket"""
        if not self.process or self.process.poll() is not None:
            self._is_playing = False
            return False

        socket_path = self._get_socket_path()
        if not os.path.exists(socket_path):
            return False

        try:
            import socket
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.settimeout(1.0)
            sock.connect(socket_path)

            msg = json.dumps({"command": command}) + "\n"
            sock.send(msg.encode())
            sock.close()
            return True
        except Exception:
            # Check if process is still alive
            if self.process and self.process.poll() is not None:
                self._is_playing = False
            return False

    def pause_resume(self):
        """Toggle pause/resume via mpv IPC"""
        if not self._is_playing:
            return False

        # First, get current pause state
        try:
            import socket
            import json
            socket_path = self._get_socket_path()
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.settimeout(1.0)
            sock.connect(socket_path)
            sock.send(b'{"command": ["get_property", "pause"]}\n')
            response = sock.recv(1024).decode()
            sock.close()

            if response.strip():
                try:
                    resp_json = json.loads(response.strip())
                    if "data" in resp_json:
                        current_paused = bool(resp_json["data"])
                        # Toggle the state
                        new_state = not current_paused
                        success = self._send_command(["set_property", "pause", new_state])
                        if success:
                            self._is_paused = new_state
                        return success
                except Exception:
                    pass
        except Exception:
            pass

        # Fallback: just send cycle pause
        success = self._send_command(["cycle", "pause"])
        if success:
            self._is_paused = not self._is_paused
        return success

    def stop(self):
        """Stop playback"""
        if self.process:
            try:
                # Try graceful quit first
                self._send_command(["quit"])
                # Wait for process to end
                try:
                    self.process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    # Force kill if needed
                    import signal
                    try:
                        os.killpg(os.getpgid(self.process.pid), signal.SIGTERM)
                        self.process.wait(timeout=1)
                    except Exception:
                        try:
                            os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
                        except Exception:
                            pass
            except Exception:
                pass
            finally:
                self.process = None
                self._is_playing = False
                self._is_paused = False
                self._current_file = None

                # Clean up socket
                if self.socket_path and os.path.exists(self.socket_path):
                    try:
                        os.unlink(self.socket_path)
                    except Exception:
                        pass
                self.socket_path = None

    def is_playing(self):
        if not self.process:
            return False
        if self.process.poll() is not None:
            self._is_playing = False
            return False
        return self._is_playing

    def is_paused(self):
        return self._is_paused

    def get_current_file(self):
        return self._current_file
