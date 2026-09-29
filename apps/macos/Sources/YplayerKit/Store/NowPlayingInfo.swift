import Foundation

/// What the system's Now Playing surfaces show for the current song.
public struct NowPlayingInfo: Equatable, Sendable {
    public var title: String
    public var artist: String?
    public var duration: Double?
    /// The `PositionClock` position at the time of the mapping.
    public var elapsed: Double
    /// 1 while playing, else 0.
    public var rate: Double
    public var isPlaying: Bool
    public var artworkPath: String?

    /// The info for `player` and its `track`; nil when nothing is playing.
    public static func make(player: PlayerState?, track: Track?, now: Date = Date())
        -> NowPlayingInfo?
    {
        guard let player, let track, player.state != .stopped else { return nil }
        let isPlaying = player.state == .playing
        return NowPlayingInfo(
            title: track.title,
            artist: track.uploader,
            duration: player.duration,
            elapsed: PositionClock.position(player, now: now),
            rate: isPlaying ? 1 : 0,
            isPlaying: isPlaying,
            artworkPath: track.thumbPath)
    }
}
