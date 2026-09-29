import Foundation

public enum YouTubeURLError: Error, Equatable, Sendable {
    case notYouTube, playlistOnly, noVideoID
}

public enum YouTubeURL {
    public static func videoID(from input: String) -> Result<String, YouTubeURLError> {
        let input = input.trimmingCharacters(in: .whitespacesAndNewlines)
        if isVideoID(input) {
            return .success(input)
        }

        let lower = input.lowercased()
        let rest: Substring =
            if lower.hasPrefix("https://") {
                input.dropFirst(8)
            } else if lower.hasPrefix("http://") {
                input.dropFirst(7)
            } else {
                input[...]
            }

        let hostEnd = rest.firstIndex { "/?#".contains($0) } ?? rest.endIndex
        var host = String(rest[..<hostEnd].lowercased().prefix { $0 != ":" })
        if let prefix = ["www.", "m.", "music."].first(where: { host.hasPrefix($0) }) {
            host = String(host.dropFirst(prefix.count))
        }

        let tail = rest[hostEnd...].prefix { $0 != "#" }
        let path: Substring
        let query: Substring
        if let mark = tail.firstIndex(of: "?") {
            path = tail[..<mark]
            query = tail[tail.index(after: mark)...]
        } else {
            path = tail
            query = ""
        }
        let segments = path.split(separator: "/").map(String.init)

        switch host {
        case "youtu.be":
            return videoID(candidate: segments.first)
        case "youtube.com":
            switch segments.first {
            case "watch"?:
                if let v = queryParam(query, "v") {
                    return videoID(candidate: v)
                }
                return .failure(queryParam(query, "list") != nil ? .playlistOnly : .noVideoID)
            case "playlist"? where queryParam(query, "list") != nil:
                return .failure(.playlistOnly)
            case "shorts"?, "live"?, "embed"?:
                return videoID(candidate: segments.dropFirst().first)
            default:
                return .failure(.noVideoID)
            }
        default:
            return .failure(.notYouTube)
        }
    }

    private static func videoID(candidate: String?) -> Result<String, YouTubeURLError> {
        guard let candidate, isVideoID(candidate) else { return .failure(.noVideoID) }
        return .success(candidate)
    }

    private static func isVideoID(_ s: String) -> Bool {
        s.utf8.count == 11
            && s.utf8.allSatisfy { byte in
                (byte >= UInt8(ascii: "a") && byte <= UInt8(ascii: "z"))
                    || (byte >= UInt8(ascii: "A") && byte <= UInt8(ascii: "Z"))
                    || (byte >= UInt8(ascii: "0") && byte <= UInt8(ascii: "9"))
                    || byte == UInt8(ascii: "_") || byte == UInt8(ascii: "-")
            }
    }

    private static func queryParam(_ query: Substring, _ key: String) -> String? {
        for pair in query.split(separator: "&", omittingEmptySubsequences: false) {
            if let equals = pair.firstIndex(of: "="), pair[..<equals] == key {
                return String(pair[pair.index(after: equals)...])
            }
        }
        return nil
    }
}
