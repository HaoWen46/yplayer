import Foundation

/// One request per wire command of protocol v1.
public enum Command: Equatable, Sendable {
    case hello(protocol: Int)
    case subscribe
    case libraryGet
    case now
    case add(url: String, album: AlbumRef?, play: Bool)
    case play(trackID: String, context: ContextRef)
    case pause
    case resume
    case toggle
    case stop
    case next
    case prev
    case seek(position: Double)
    case volume(value: Double)
    case loop(mode: LoopMode)
    case queuePlayNext(trackID: String)
    case albumCreate(name: String)
    case albumRename(albumID: Int64, name: String)
    case albumDelete(albumID: Int64)
    case albumAdd(albumID: Int64, trackID: String)
    case albumRemove(albumID: Int64, trackID: String)
    case albumReorder(albumID: Int64, trackIDs: [String])
    case trackDelete(trackID: String, toTrash: Bool)
    case trackRename(trackID: String, title: String)
    case trackRetry(trackID: String)
    case rescan
    case lyrics(trackID: String)
    case settingsGet
    /// `settings.set` with the fields given; `apiKey: .some(nil)` sends null (removes the key).
    case settingsSet(levelLoudness: Bool? = nil, apiKey: String?? = nil)
    case libraryMove(to: String)
    case queueGet
    case queueRemove(section: QueueSection, index: Int, trackID: String)
    /// `to` is the entry's final index in `next`.
    case queueMove(from: Int, to: Int, trackID: String)
    case queueClear
    case queueJump(section: QueueSection, index: Int, trackID: String)

    /// The wire `cmd` value.
    var name: String {
        switch self {
        case .hello: "hello"
        case .subscribe: "subscribe"
        case .libraryGet: "library.get"
        case .now: "now"
        case .add: "add"
        case .play: "play"
        case .pause: "pause"
        case .resume: "resume"
        case .toggle: "toggle"
        case .stop: "stop"
        case .next: "next"
        case .prev: "prev"
        case .seek: "seek"
        case .volume: "volume"
        case .loop: "loop"
        case .queuePlayNext: "queue.play_next"
        case .albumCreate: "album.create"
        case .albumRename: "album.rename"
        case .albumDelete: "album.delete"
        case .albumAdd: "album.add"
        case .albumRemove: "album.remove"
        case .albumReorder: "album.reorder"
        case .trackDelete: "track.delete"
        case .trackRename: "track.rename"
        case .trackRetry: "track.retry"
        case .rescan: "rescan"
        case .lyrics: "lyrics"
        case .settingsGet: "settings.get"
        case .settingsSet: "settings.set"
        case .libraryMove: "library.move"
        case .queueGet: "queue.get"
        case .queueRemove: "queue.remove"
        case .queueMove: "queue.move"
        case .queueClear: "queue.clear"
        case .queueJump: "queue.jump"
        }
    }

    /// The request as one compact JSON line terminated by `\n`.
    public func line(id: UInt64) -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = .withoutEscapingSlashes
        // Envelope encoding cannot throw: non-finite numbers are written as null (as serde_json does).
        var data = (try? encoder.encode(Envelope(id: id, command: self))) ?? Data()
        data.append(UInt8(ascii: "\n"))
        return data
    }
}

private struct Envelope: Encodable {
    let id: UInt64
    let command: Command

    enum CodingKeys: String, CodingKey {
        case id, cmd, `protocol`, url, album, play, context, position, value, mode, name, title
        case section, index, from, to
        case trackID = "track_id"
        case albumID = "album_id"
        case trackIDs = "track_ids"
        case toTrash = "to_trash"
        case levelLoudness = "level_loudness"
        case apiKey = "api_key"
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(command.name, forKey: .cmd)
        switch command {
        case .subscribe, .libraryGet, .now, .pause, .resume, .toggle, .stop, .next, .prev, .rescan,
            .queueGet, .queueClear:
            break
        case .hello(let version):
            try container.encode(version, forKey: .protocol)
        case .add(let url, let album, let play):
            try container.encode(url, forKey: .url)
            if let album {
                try container.encode(album, forKey: .album)
            } else {
                try container.encodeNil(forKey: .album)
            }
            try container.encode(play, forKey: .play)
        case .play(let trackID, let context):
            try container.encode(trackID, forKey: .trackID)
            try container.encode(context, forKey: .context)
        case .seek(let position):
            try encodeNumber(position, forKey: .position, in: &container)
        case .volume(let value):
            try encodeNumber(value, forKey: .value, in: &container)
        case .loop(let mode):
            try container.encode(mode, forKey: .mode)
        case .albumCreate(let name):
            try container.encode(name, forKey: .name)
        case .albumRename(let albumID, let name):
            try container.encode(albumID, forKey: .albumID)
            try container.encode(name, forKey: .name)
        case .albumDelete(let albumID):
            try container.encode(albumID, forKey: .albumID)
        case .albumAdd(let albumID, let trackID), .albumRemove(let albumID, let trackID):
            try container.encode(albumID, forKey: .albumID)
            try container.encode(trackID, forKey: .trackID)
        case .albumReorder(let albumID, let trackIDs):
            try container.encode(albumID, forKey: .albumID)
            try container.encode(trackIDs, forKey: .trackIDs)
        case .trackDelete(let trackID, let toTrash):
            try container.encode(trackID, forKey: .trackID)
            try container.encode(toTrash, forKey: .toTrash)
        case .trackRename(let trackID, let title):
            try container.encode(trackID, forKey: .trackID)
            try container.encode(title, forKey: .title)
        case .queuePlayNext(let trackID), .trackRetry(let trackID), .lyrics(let trackID):
            try container.encode(trackID, forKey: .trackID)
        case .settingsGet:
            break
        case .settingsSet(let levelLoudness, let apiKey):
            try container.encodeIfPresent(levelLoudness, forKey: .levelLoudness)
            switch apiKey {
            case .none: break
            case .some(.none): try container.encodeNil(forKey: .apiKey)
            case .some(.some(let key)): try container.encode(key, forKey: .apiKey)
            }
        case .libraryMove(let to):
            try container.encode(to, forKey: .to)
        case .queueRemove(let section, let index, let trackID),
            .queueJump(let section, let index, let trackID):
            try container.encode(section, forKey: .section)
            try container.encode(index, forKey: .index)
            try container.encode(trackID, forKey: .trackID)
        case .queueMove(let from, let to, let trackID):
            try container.encode(from, forKey: .from)
            try container.encode(to, forKey: .to)
            try container.encode(trackID, forKey: .trackID)
        }
    }

    private func encodeNumber(
        _ value: Double, forKey key: CodingKeys,
        in container: inout KeyedEncodingContainer<CodingKeys>
    ) throws {
        if value.isFinite {
            try container.encode(value, forKey: key)
        } else {
            try container.encodeNil(forKey: key)
        }
    }
}
