import Foundation

public enum TrackState: String, Codable, Equatable, Sendable {
    case complete, downloading, failed
}

public enum LoopMode: String, Codable, Equatable, Sendable {
    case none, single, all, shuffle
}

public enum PlayState: String, Codable, Equatable, Sendable {
    case playing, paused, stopped
}

public enum DownloadPhase: String, Codable, Equatable, Sendable {
    case fetching, downloading, done, failed, cancelled
}

public enum Severity: String, Codable, Equatable, Sendable {
    case info, warn, error
}

public struct Track: Codable, Equatable, Identifiable, Sendable {
    public var id: String
    public var title: String
    public var uploader: String?
    public var duration: Int?
    public var webpageURL: String?
    public var audioPath: String?
    public var format: String?
    public var fileSize: Int64?
    public var addedAt: Int64?
    public var lastPlayed: Int64?
    public var state: TrackState
    public var thumbPath: String?

    enum CodingKeys: String, CodingKey {
        case id, title, uploader, duration, format, state
        case webpageURL = "webpage_url"
        case audioPath = "audio_path"
        case fileSize = "file_size"
        case addedAt = "added_at"
        case lastPlayed = "last_played"
        case thumbPath = "thumb_path"
    }

    public init(
        id: String, title: String, uploader: String?, duration: Int?, webpageURL: String?,
        audioPath: String?, format: String?, fileSize: Int64?, addedAt: Int64?,
        lastPlayed: Int64?, state: TrackState, thumbPath: String?
    ) {
        self.id = id
        self.title = title
        self.uploader = uploader
        self.duration = duration
        self.webpageURL = webpageURL
        self.audioPath = audioPath
        self.format = format
        self.fileSize = fileSize
        self.addedAt = addedAt
        self.lastPlayed = lastPlayed
        self.state = state
        self.thumbPath = thumbPath
    }
}

public struct Album: Codable, Equatable, Identifiable, Sendable {
    public var id: Int64
    public var name: String
    public var trackIDs: [String]
    public var createdAt: Int64
    public var lastUsedAt: Int64?

    enum CodingKeys: String, CodingKey {
        case id, name
        case trackIDs = "track_ids"
        case createdAt = "created_at"
        case lastUsedAt = "last_used_at"
    }

    public init(id: Int64, name: String, trackIDs: [String], createdAt: Int64, lastUsedAt: Int64?) {
        self.id = id
        self.name = name
        self.trackIDs = trackIDs
        self.createdAt = createdAt
        self.lastUsedAt = lastUsedAt
    }
}

/// Wire shape `{"album_id": N}` or `{"library": true}`.
public enum ContextRef: Codable, Equatable, Sendable {
    case album(Int64)
    case library

    private enum CodingKeys: String, CodingKey {
        case albumID = "album_id"
        case library
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if let id = try container.decodeIfPresent(Int64.self, forKey: .albumID) {
            self = .album(id)
        } else if try container.decodeIfPresent(Bool.self, forKey: .library) == true {
            self = .library
        } else {
            throw DecodingError.dataCorrupted(
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "context must be {album_id} or {library: true}"))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .album(let id): try container.encode(id, forKey: .albumID)
        case .library: try container.encode(true, forKey: .library)
        }
    }
}

/// Wire shape `{"id": N}` or `{"name": "..."}`.
public enum AlbumRef: Codable, Equatable, Sendable {
    case id(Int64)
    case name(String)

    private enum CodingKeys: String, CodingKey {
        case id, name
    }

    public init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if let id = try container.decodeIfPresent(Int64.self, forKey: .id) {
            self = .id(id)
        } else {
            self = .name(try container.decode(String.self, forKey: .name))
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .id(let id): try container.encode(id, forKey: .id)
        case .name(let name): try container.encode(name, forKey: .name)
        }
    }
}

public struct PlayerState: Codable, Equatable, Sendable {
    public var state: PlayState
    public var trackID: String?
    public var context: ContextRef?
    public var position: Double
    /// Wall-clock ms when `position` was sampled.
    public var atMs: Int64
    public var duration: Double?
    public var volume: Double
    public var loopMode: LoopMode

    enum CodingKeys: String, CodingKey {
        case state, context, position, duration, volume
        case trackID = "track_id"
        case atMs = "at_ms"
        case loopMode = "loop"
    }

    public init(
        state: PlayState, trackID: String?, context: ContextRef?, position: Double, atMs: Int64,
        duration: Double?, volume: Double, loopMode: LoopMode
    ) {
        self.state = state
        self.trackID = trackID
        self.context = context
        self.position = position
        self.atMs = atMs
        self.duration = duration
        self.volume = volume
        self.loopMode = loopMode
    }
}

public struct DownloadEvent: Codable, Equatable, Sendable {
    public var trackID: String
    public var phase: DownloadPhase
    public var bytes: UInt64?
    public var total: UInt64?
    public var error: String?

    enum CodingKeys: String, CodingKey {
        case phase, bytes, total, error
        case trackID = "track_id"
    }

    public init(
        trackID: String, phase: DownloadPhase, bytes: UInt64?, total: UInt64?, error: String?
    ) {
        self.trackID = trackID
        self.phase = phase
        self.bytes = bytes
        self.total = total
        self.error = error
    }
}

public struct Toast: Codable, Equatable, Sendable {
    public var severity: Severity
    public var message: String

    public init(severity: Severity, message: String) {
        self.severity = severity
        self.message = message
    }
}
