import os
from yplayer.core import is_url, path_for
from yplayer.utils import normalize_ext

def test_is_url():
    assert is_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
    assert is_url("youtu.be/dQw4w9WgXcQ")
    assert not is_url("rick astley never gonna give you up")

def test_path_for(tmp_path):
    p = path_for(str(tmp_path), "abc123", "MP3")
    assert os.path.basename(p) == "abc123.mp3"

def test_normalize_ext():
    assert normalize_ext(".Mp3") == "mp3"
    assert normalize_ext(" m4a ") == "m4a"
