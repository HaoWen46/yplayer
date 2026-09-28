import AppKit
import ImageIO
import MediaPlayer
import Observation
import YplayerKit

/// Publishes the current song to the system's Now Playing surfaces and routes media keys and
/// remote commands to `AppModel`. Updates only when the player state or the current track
/// changes; macOS extrapolates the elapsed time from the playback rate.
@MainActor
final class NowPlayingController {
    private let model: AppModel
    private var shownPlayer: PlayerState?
    private var shownTrack: Track?
    private var artworkPath: String?
    private var artwork: MPMediaItemArtwork?

    init(model: AppModel) {
        self.model = model
        Self.registerCommands(model: model)
        observe()
    }

    /// Publishes on a change of the player or the current track; re-arms itself after every
    /// change.
    private func observe() {
        let (player, track) = withObservationTracking {
            (model.store.player, model.store.currentTrack)
        } onChange: {
            Task { @MainActor in self.observe() }
        }
        guard player != shownPlayer || track != shownTrack else { return }
        shownPlayer = player
        shownTrack = track
        publish(NowPlayingInfo.make(player: player, track: track))
    }

    private func publish(_ info: NowPlayingInfo?) {
        let center = MPNowPlayingInfoCenter.default()
        guard let info else {
            center.nowPlayingInfo = nil
            center.playbackState = .stopped
            return
        }
        if info.artworkPath != artworkPath {
            artworkPath = info.artworkPath
            artwork = info.artworkPath.flatMap(Self.artwork(path:))
        }
        var properties: [String: Any] = [
            MPMediaItemPropertyTitle: info.title,
            MPNowPlayingInfoPropertyElapsedPlaybackTime: info.elapsed,
            MPNowPlayingInfoPropertyPlaybackRate: info.rate,
        ]
        properties[MPMediaItemPropertyArtist] = info.artist
        properties[MPMediaItemPropertyPlaybackDuration] = info.duration
        properties[MPMediaItemPropertyArtwork] = artwork
        center.nowPlayingInfo = properties
        center.playbackState = info.isPlaying ? .playing : .paused
    }

    /// The image at `path` scaled to at most 600 px, or nil when it cannot be read. Built outside
    /// the main actor because MediaPlayer may call the request handler on any thread.
    private nonisolated static func artwork(path: String) -> MPMediaItemArtwork? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 600,
        ]
        guard let source = CGImageSourceCreateWithURL(URL(filePath: path) as CFURL, nil),
            let cgImage = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
        else { return nil }
        let size = NSSize(width: cgImage.width, height: cgImage.height)
        let image = NSImage(cgImage: cgImage, size: size)
        return MPMediaItemArtwork(boundsSize: size) { _ in image }
    }

    /// Registers the remote command handlers. Built outside the main actor because MediaPlayer
    /// may call them on any thread; each hops to the main actor to run an intent.
    private nonisolated static func registerCommands(model: AppModel) {
        let center = MPRemoteCommandCenter.shared()
        center.playCommand.addTarget { _ in
            Task { @MainActor in
                if model.store.player?.state != .playing { await model.toggle() }
            }
            return .success
        }
        center.pauseCommand.addTarget { _ in
            Task { @MainActor in
                if model.store.player?.state == .playing { await model.toggle() }
            }
            return .success
        }
        center.togglePlayPauseCommand.addTarget { _ in
            Task { @MainActor in await model.toggle() }
            return .success
        }
        center.nextTrackCommand.addTarget { _ in
            Task { @MainActor in await model.next() }
            return .success
        }
        center.previousTrackCommand.addTarget { _ in
            Task { @MainActor in await model.prev() }
            return .success
        }
        center.changePlaybackPositionCommand.addTarget { event in
            guard let event = event as? MPChangePlaybackPositionCommandEvent else {
                return .commandFailed
            }
            let position = event.positionTime
            Task { @MainActor in await model.seek(to: position) }
            return .success
        }
    }
}
