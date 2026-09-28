import Foundation

public enum DropPayload {
    public static func url(from strings: [String]) -> String? {
        for string in strings {
            let line = string.trimmingCharacters(in: .whitespacesAndNewlines)
                .prefix { !$0.isNewline }
                .trimmingCharacters(in: .whitespaces)
            if case .success = YouTubeURL.videoID(from: line) {
                return line
            }
        }
        return nil
    }
}
