import Foundation
import Testing

@testable import YplayerKit

private func track(_ title: String, uploader: String? = "ZUTOMAYO") -> Track {
    Track(
        id: "a", title: title, uploader: uploader, duration: 231, webpageURL: nil,
        audioPath: nil, format: nil, fileSize: nil, addedAt: nil, lastPlayed: nil,
        state: .complete, thumbPath: "/tmp/cache/a/thumb.jpg")
}

private func player(_ state: PlayState, trackID: String? = "a", duration: Double? = 231)
    -> PlayerState
{
    PlayerState(
        state: state, trackID: trackID, context: .library, position: 62, atMs: 1_000_000,
        duration: duration, volume: 80, loopMode: .none)
}

private let now = Date(timeIntervalSince1970: 1_002.5)

@Test func playingExtrapolatesElapsed() {
    let info = NowPlayingInfo.make(player: player(.playing), track: track("Song"), now: now)
    #expect(
        info
            == NowPlayingInfo(
                title: "Song", artist: "ZUTOMAYO", duration: 231, elapsed: 64.5, rate: 1,
                isPlaying: true, artworkPath: "/tmp/cache/a/thumb.jpg"))
}

@Test func pausedKeepsPositionAndZeroRate() {
    let info = NowPlayingInfo.make(player: player(.paused), track: track("Song"), now: now)
    #expect(info?.elapsed == 62)
    #expect(info?.rate == 0)
    #expect(info?.isPlaying == false)
    #expect(info?.duration == 231)
}

@Test func nothingPlayingIsNil() {
    #expect(NowPlayingInfo.make(player: nil, track: nil, now: now) == nil)
    #expect(NowPlayingInfo.make(player: player(.playing), track: nil, now: now) == nil)
    #expect(
        NowPlayingInfo.make(player: player(.stopped, trackID: nil), track: nil, now: now) == nil)
    #expect(NowPlayingInfo.make(player: player(.stopped), track: track("Song"), now: now) == nil)
}

@Test func missingDurationIsNilAndElapsedUnclamped() {
    let info = NowPlayingInfo.make(
        player: player(.playing, duration: nil), track: track("Song", uploader: nil), now: now)
    #expect(info?.duration == nil)
    #expect(info?.elapsed == 64.5)
    #expect(info?.artist == nil)
}

@Test func cjkTitleIsKept() {
    let title = "ずっと真夜中でいいのに。『秒針を噛む』MV"
    let info = NowPlayingInfo.make(
        player: player(.playing), track: track(title, uploader: "ずっと真夜中でいいのに。 ZUTOMAYO"),
        now: now)
    #expect(info?.title == title)
    #expect(info?.artist == "ずっと真夜中でいいのに。 ZUTOMAYO")
}
