import Foundation

/// A server-pushed line with an `event` key.
public enum Event: Decodable, Equatable, Sendable {
    case player(PlayerState)
    case trackUpsert(Track)
    case trackRemoved(String)
    case albumUpsert(Album)
    case albumRemoved(Int64)
    case download(DownloadEvent)
    case toast(Toast)
    case resync
    /// An event name this client does not know; never an error (forward compatibility).
    case unknown(String)

    private enum CodingKeys: String, CodingKey {
        case event, track, album
        case trackID = "track_id"
        case albumID = "album_id"
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let name = try container.decode(String.self, forKey: .event)
        switch name {
        case "player": self = .player(try PlayerState(from: decoder))
        case "track.upsert": self = .trackUpsert(try container.decode(Track.self, forKey: .track))
        case "track.removed":
            self = .trackRemoved(try container.decode(String.self, forKey: .trackID))
        case "album.upsert": self = .albumUpsert(try container.decode(Album.self, forKey: .album))
        case "album.removed":
            self = .albumRemoved(try container.decode(Int64.self, forKey: .albumID))
        case "download": self = .download(try DownloadEvent(from: decoder))
        case "toast": self = .toast(try Toast(from: decoder))
        case "resync": self = .resync
        default: self = .unknown(name)
        }
    }
}
