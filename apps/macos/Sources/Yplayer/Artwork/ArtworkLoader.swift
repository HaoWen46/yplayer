import ImageIO
import SwiftUI

/// Decodes artwork off the main actor at display size (ImageIO thumbnails), cached by path and
/// pixel size.
actor ArtworkLoader {
    static let shared = ArtworkLoader()

    private let cache: NSCache<NSString, CGImage> = {
        let cache = NSCache<NSString, CGImage>()
        cache.countLimit = 300
        cache.totalCostLimit = 64 * 1024 * 1024
        return cache
    }()

    /// The image at `path` decoded to at most `pointSize × scale` pixels; nil when the file is
    /// missing or unreadable.
    func image(for path: String, pointSize: CGFloat, scale: CGFloat) async -> CGImage? {
        let maxPixelSize = Int((pointSize * scale).rounded(.up))
        let key = "\(path)#\(maxPixelSize)" as NSString
        if let cached = cache.object(forKey: key) { return cached }
        guard let source = CGImageSourceCreateWithURL(URL(filePath: path) as CFURL, nil) else {
            return nil
        }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixelSize,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
        else { return nil }
        cache.setObject(image, forKey: key, cost: image.bytesPerRow * image.height)
        return image
    }

    /// Empties the cache (the popover closed; images reload quickly on the next open).
    func purge() {
        cache.removeAllObjects()
    }
}

/// A track's artwork in a rounded square; `music.note` on a tinted rounded rect while loading or
/// when there is none.
struct ArtworkView: View {
    let path: String?
    let size: CGFloat
    var cornerRadius: CGFloat = 8
    @Environment(\.displayScale) private var displayScale
    @ViewState private var image: CGImage? = nil

    var body: some View {
        RoundedRectangle(cornerRadius: cornerRadius)
            .fill(.tint.opacity(0.15))
            .overlay {
                if let image {
                    Image(decorative: image, scale: displayScale)
                        .resizable()
                        .scaledToFill()
                } else {
                    Image(systemName: "music.note")
                        .font(.system(size: size * 0.4, weight: .medium))
                        .foregroundStyle(.tint)
                }
            }
            .frame(width: size, height: size)
            .clipShape(.rect(cornerRadius: cornerRadius))
            .task(id: path) {
                image = nil
                guard let path else { return }
                image = await ArtworkLoader.shared.image(
                    for: path, pointSize: size, scale: displayScale)
            }
    }
}
