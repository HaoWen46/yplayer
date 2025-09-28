# yplayer/albums.py
import json
import os
from typing import List, Dict, Optional
from .core import find_existing, DEFAULT_CACHE_DIR

class AlbumManager:
    def __init__(self, cache_dir: str = None):
        self.cache_dir = cache_dir or DEFAULT_CACHE_DIR
        self.albums_dir = os.path.join(self.cache_dir, "albums")
        os.makedirs(self.albums_dir, exist_ok=True)

    def create_album(self, name: str, description: str = "", tracks: List[Dict] = None) -> bool:
        """Create a new album"""
        if not name:
            return False

        album_file = os.path.join(self.albums_dir, f"{self._sanitize_name(name)}.album.json")
        if os.path.exists(album_file):
            return False  # Album already exists

        album_data = {
            "name": name,
            "description": description,
            "tracks": tracks or []
        }

        try:
            with open(album_file, 'w', encoding='utf-8') as f:
                json.dump(album_data, f, ensure_ascii=False, indent=2)
            return True
        except Exception:
            return False

    def list_albums(self) -> List[Dict]:
        """List all albums with basic info"""
        albums = []
        try:
            for filename in os.listdir(self.albums_dir):
                if filename.endswith('.album.json'):
                    filepath = os.path.join(self.albums_dir, filename)
                    try:
                        with open(filepath, 'r', encoding='utf-8') as f:
                            data = json.load(f)
                            albums.append({
                                "name": data.get("name", filename.replace('.album.json', '')),
                                "description": data.get("description", ""),
                                "track_count": len(data.get("tracks", [])),
                                "path": filepath
                            })
                    except Exception:
                        continue
        except Exception:
            pass

        # Sort alphabetically
        albums.sort(key=lambda x: x["name"].lower())
        return albums

    def get_album_tracks(self, album_path: str) -> List[Dict]:
        """Get tracks for a specific album, resolving to cached files"""
        try:
            with open(album_path, 'r', encoding='utf-8') as f:
                album_data = json.load(f)

            tracks = []
            for track_info in album_data.get("tracks", []):
                video_id = track_info.get("id")
                title = track_info.get("title", "")
                order = track_info.get("order", len(tracks) + 1)

                # Try to find cached file
                cached_path = None
                if video_id:
                    cached_path = find_existing(self.cache_dir, video_id, title)

                track = {
                    "id": video_id,
                    "title": title,
                    "uploader": track_info.get("uploader", ""),
                    "duration": track_info.get("duration"),
                    "webpage_url": track_info.get("webpage_url"),
                    "path": cached_path,
                    "order": order,
                    "type": "album_track"
                }
                tracks.append(track)

            # Sort by order
            tracks.sort(key=lambda x: x.get("order", 0))
            return tracks

        except Exception:
            return []

    def add_track_to_album(self, album_path: str, track_info: Dict) -> bool:
        """Add a track to an existing album"""
        try:
            with open(album_path, 'r', encoding='utf-8') as f:
                album_data = json.load(f)

            # Check if track already exists
            existing_ids = {t.get("id") for t in album_data.get("tracks", []) if t.get("id")}
            if track_info.get("id") in existing_ids:
                return False  # Already exists

            # Add new track
            album_data.setdefault("tracks", []).append(track_info)

            with open(album_path, 'w', encoding='utf-8') as f:
                json.dump(album_data, f, ensure_ascii=False, indent=2)
            return True

        except Exception:
            return False

    def remove_track_from_album(self, album_path: str, track_id: str) -> bool:
        """Remove a track from an album"""
        try:
            with open(album_path, 'r', encoding='utf-8') as f:
                album_data = json.load(f)

            original_count = len(album_data.get("tracks", []))
            album_data["tracks"] = [t for t in album_data.get("tracks", []) if t.get("id") != track_id]

            if len(album_data["tracks"]) == original_count:
                return False  # Track not found

            with open(album_path, 'w', encoding='utf-8') as f:
                json.dump(album_data, f, ensure_ascii=False, indent=2)
            return True

        except Exception:
            return False

    def _sanitize_name(self, name: str) -> str:
        """Sanitize album name for filename"""
        import re
        return re.sub(r'[^\w\s-]', '', name).strip().replace(' ', '_')

    def get_album_by_name(self, name: str) -> Optional[str]:
        """Get album file path by name"""
        sanitized = self._sanitize_name(name)
        album_file = os.path.join(self.albums_dir, f"{sanitized}.album.json")
        if os.path.exists(album_file):
            return album_file
        return None
