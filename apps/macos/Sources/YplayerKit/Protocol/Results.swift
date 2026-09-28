import Foundation

public struct HelloResult: Decodable, Equatable, Sendable {
    public var `protocol`: Int
    public var serverVersion: String

    enum CodingKeys: String, CodingKey {
        case `protocol`
        case serverVersion = "server_version"
    }

    public init(protocol: Int, serverVersion: String) {
        self.protocol = `protocol`
        self.serverVersion = serverVersion
    }
}

public struct SubscribeResult: Decodable, Equatable, Sendable {
    public var player: PlayerState
    public var libraryVersion: UInt64

    enum CodingKeys: String, CodingKey {
        case player
        case libraryVersion = "library_version"
    }

    public init(player: PlayerState, libraryVersion: UInt64) {
        self.player = player
        self.libraryVersion = libraryVersion
    }
}

public struct LibrarySnapshot: Decodable, Equatable, Sendable {
    public var tracks: [Track]
    public var albums: [Album]
    public var libraryVersion: UInt64

    enum CodingKeys: String, CodingKey {
        case tracks, albums
        case libraryVersion = "library_version"
    }

    public init(tracks: [Track], albums: [Album], libraryVersion: UInt64) {
        self.tracks = tracks
        self.albums = albums
        self.libraryVersion = libraryVersion
    }
}

public struct NowResult: Decodable, Equatable, Sendable {
    public var player: PlayerState
    public var track: Track?

    public init(player: PlayerState, track: Track?) {
        self.player = player
        self.track = track
    }
}

public struct AddResult: Decodable, Equatable, Sendable {
    public var trackID: String
    public var albumID: Int64
    public var wasNew: Bool
    public var wasInAlbum: Bool

    enum CodingKeys: String, CodingKey {
        case trackID = "track_id"
        case albumID = "album_id"
        case wasNew = "was_new"
        case wasInAlbum = "was_in_album"
    }

    public init(trackID: String, albumID: Int64, wasNew: Bool, wasInAlbum: Bool) {
        self.trackID = trackID
        self.albumID = albumID
        self.wasNew = wasNew
        self.wasInAlbum = wasInAlbum
    }
}

public struct AlbumCreateResult: Decodable, Equatable, Sendable {
    public var album: Album

    public init(album: Album) {
        self.album = album
    }
}

public struct LyricLine: Decodable, Equatable, Sendable {
    public var tMs: Int64?
    public var text: String

    enum CodingKeys: String, CodingKey {
        case text
        case tMs = "t_ms"
    }

    public init(tMs: Int64?, text: String) {
        self.tMs = tMs
        self.text = text
    }
}

/// `{"synced": Bool, "lines": [...]}` or `{"missing": true}`.
public enum LyricsResult: Decodable, Equatable, Sendable {
    case lines(synced: Bool, [LyricLine])
    case missing

    private enum CodingKeys: String, CodingKey {
        case synced, lines, missing
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if try container.decodeIfPresent(Bool.self, forKey: .missing) == true {
            self = .missing
        } else {
            self = .lines(
                synced: try container.decode(Bool.self, forKey: .synced),
                try container.decode([LyricLine].self, forKey: .lines))
        }
    }
}

/// The `{}` result of commands that return nothing.
public struct EmptyResult: Decodable, Equatable, Sendable {
    public init() {}
}
