import Foundation

public enum PositionClock {
    /// The playback position at `now`, extrapolated from `p.atMs` while playing.
    public static func position(_ p: PlayerState, now: Date) -> Double {
        guard p.state == .playing else { return p.position }
        let elapsed = now.timeIntervalSince1970 - Double(p.atMs) / 1000
        let position = max(0, p.position + elapsed)
        guard let duration = p.duration else { return position }
        return min(position, duration)
    }
}

public enum LyricsTimeline {
    /// The index of the last line with `tMs <= seconds`.
    public static func index(_ lines: [LyricLine], at seconds: Double) -> Int? {
        lines.lastIndex { line in
            guard let tMs = line.tMs else { return false }
            return Double(tMs) <= seconds * 1000
        }
    }
}
