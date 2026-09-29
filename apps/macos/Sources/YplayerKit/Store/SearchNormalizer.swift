import Foundation

public enum SearchNormalizer {
    /// Case-, diacritic- and width-insensitive form with hiragana folded to katakana.
    public static func fold(_ text: String) -> String {
        let folded = text.folding(
            options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
        var bytes = Array(folded.utf8)
        var index = 0
        while index + 2 < bytes.count {
            guard bytes[index] == 0xE3 else {
                index += 1
                continue
            }
            // UTF-8 E3 xx yy: hiragana U+3041–U+3096 and U+309D–U+309E are katakana − U+0060.
            let last = bytes[index + 2]
            switch (bytes[index + 1], last) {
            case (0x81, 0x81...0x9F):
                bytes[index + 1] = 0x82
                bytes[index + 2] = last + 0x20
            case (0x81, 0xA0...0xBF):
                bytes[index + 1] = 0x83
                bytes[index + 2] = last - 0x20
            case (0x82, 0x80...0x96), (0x82, 0x9D...0x9E):
                bytes[index + 1] = 0x83
                bytes[index + 2] = last + 0x20
            default:
                break
            }
            index += 3
        }
        return String(decoding: bytes, as: UTF8.self)
    }
}
