import shutil
import subprocess
import sys
from typing import List, Optional

class Colors:
    RESET      = "\033[0m"
    BOLD       = "\033[1m"
    DIM        = "\033[2m"
    RESET_BOLD = "\033[22m"

    # Foreground colors
    RED     = "\033[31m"
    GREEN   = "\033[32m"
    YELLOW  = "\033[33m"
    BLUE    = "\033[34m"
    MAGENTA = "\033[35m"
    CYAN    = "\033[36m"
    WHITE   = "\033[37m"

    # Bright variants (nice on dark background)
    BRIGHT_RED     = "\033[91m"
    BRIGHT_GREEN   = "\033[92m"
    BRIGHT_YELLOW  = "\033[93m"
    BRIGHT_BLUE    = "\033[94m"
    BRIGHT_MAGENTA = "\033[95m"
    BRIGHT_CYAN    = "\033[96m"
    BRIGHT_WHITE   = "\033[97m"


def which(prog: str) -> Optional[str]:
    return shutil.which(prog)

def die(msg: str, code: int = 1):
    sys.stderr.write(f"\x1b[31merror:\x1b[0m {msg}\n")
    raise SystemExit(code)

def info(msg: str):
    sys.stderr.write(f"\x1b[36minfo:\x1b[0m {msg}\n")

def run(cmd: List[str], check: bool = True, capture: bool = False):
    """Generic runner, used by playback tools (not yt-dlp anymore)."""
    kwargs = {"text": True}
    if capture:
        kwargs["stdout"] = subprocess.PIPE
        kwargs["stderr"] = subprocess.PIPE
    proc = subprocess.run(cmd, **kwargs)
    if check and proc.returncode != 0:
        if capture:
            die(proc.stderr or f"command failed: {' '.join(cmd)}")
        else:
            die(f"command failed: {' '.join(cmd)} (exit {proc.returncode})")
    return proc

def normalize_ext(fmt: str) -> str:
    return fmt.lower().strip().lstrip(".")
