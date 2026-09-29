import Foundation

public struct OrbLayout: Sendable {
    public let radius: CGFloat
    public let bubble: CGFloat
    public let gap: CGFloat
    public let core: CGFloat
    public let coreCenter: CGPoint
    public let bubbleCenters: [CGPoint]
    public let size: CGSize

    /// Bubbles fan left on an arc from 100° to 260° (evenly spaced; a single bubble at 180°) at
    /// the smallest radius of at least 96 pt that keeps adjacent bubbles, and each bubble and the
    /// core, `gap` apart.
    public init(targetCount: Int, bubble: CGFloat = 58, gap: CGFloat = 8, core: CGFloat = 72) {
        self.bubble = bubble
        self.gap = gap
        self.core = core
        let count = max(targetCount, 0)
        let step: CGFloat = count > 1 ? 160 / CGFloat(count - 1) : 0
        var radius = max(96, core / 2 + bubble / 2 + gap)
        if count > 1 {
            radius = max(radius, (bubble + gap) / (2 * sin(step / 2 * .pi / 180)))
        }
        self.radius = radius
        let offsets = (0..<count).map { index in
            let degrees: CGFloat = count == 1 ? 180 : 100 + step * CGFloat(index)
            let radians = degrees * .pi / 180
            return CGPoint(x: radius * cos(radians), y: -radius * sin(radians))
        }
        let half = bubble / 2
        let coreHalf = core / 2
        let minX = offsets.map { $0.x - half }.reduce(-coreHalf, min)
        let maxX = offsets.map { $0.x + half }.reduce(coreHalf, max)
        let minY = offsets.map { $0.y - half }.reduce(-coreHalf, min)
        let maxY = offsets.map { $0.y + half }.reduce(coreHalf, max)
        coreCenter = CGPoint(x: -minX, y: -minY)
        bubbleCenters = offsets.map { CGPoint(x: $0.x - minX, y: $0.y - minY) }
        size = CGSize(width: maxX - minX, height: maxY - minY)
    }

    public func target(at point: CGPoint) -> Int? {
        bubbleCenters.indices
            .filter { distance(point, bubbleCenters[$0]) <= bubble / 2 }
            .min { distance(point, bubbleCenters[$0]) < distance(point, bubbleCenters[$1]) }
    }

    public func coreContains(_ point: CGPoint) -> Bool {
        distance(point, coreCenter) <= core / 2
    }

    private func distance(_ a: CGPoint, _ b: CGPoint) -> CGFloat {
        hypot(a.x - b.x, a.y - b.y)
    }
}
