import Foundation

public enum SearchNormalizer {
    /// Case-, diacritic- and width-insensitive form with hiragana folded to katakana.
    public static func fold(_ text: String) -> String {
        let folded = text.folding(
            options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
        return folded.applyingTransform(.hiraganaToKatakana, reverse: false) ?? folded
    }
}
