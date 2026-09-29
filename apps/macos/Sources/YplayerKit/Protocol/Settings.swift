import Foundation

/// The service's settings: the `settings.get` and `settings.set` result and the fields of the
/// `settings` event.
public struct ServiceSettings: Codable, Equatable, Sendable {
    /// Whether every song plays at a similar loudness.
    public var levelLoudness: Bool
    /// Whether the service found ffmpeg, which measuring loudness needs.
    public var loudnessAvailable: Bool
    /// The YouTube API key `yplay search` uses; nil when none is set.
    public var apiKey: String?
    /// The music folder's full path.
    public var musicFolder: String

    enum CodingKeys: String, CodingKey {
        case levelLoudness = "level_loudness"
        case loudnessAvailable = "loudness_available"
        case apiKey = "api_key"
        case musicFolder = "music_folder"
    }

    public init(
        levelLoudness: Bool, loudnessAvailable: Bool, apiKey: String?, musicFolder: String
    ) {
        self.levelLoudness = levelLoudness
        self.loudnessAvailable = loudnessAvailable
        self.apiKey = apiKey
        self.musicFolder = musicFolder
    }
}

/// The `library.move` result `{"restarting": true}`: the service restarts to move the folder.
public struct LibraryMoveResult: Decodable, Equatable, Sendable {
    public var restarting: Bool

    public init(restarting: Bool) {
        self.restarting = restarting
    }
}

/// Music folder paths as the settings window shows and moves them.
public enum MusicFolderPath {
    /// `path` with the home folder written as `~`.
    public static func display(_ path: String, home: String = NSHomeDirectory()) -> String {
        let home = home.count > 1 && home.hasSuffix("/") ? String(home.dropLast()) : home
        if path == home { return "~" }
        guard path.hasPrefix(home + "/") else { return path }
        return "~" + path.dropFirst(home.count)
    }

    /// Where `library.move` moves `folder` when the user picks `parent`: `parent` joined with
    /// the folder's name.
    public static func moveTarget(for folder: String, into parent: String) -> String {
        URL(filePath: parent).appending(path: URL(filePath: folder).lastPathComponent)
            .path(percentEncoded: false)
    }
}
