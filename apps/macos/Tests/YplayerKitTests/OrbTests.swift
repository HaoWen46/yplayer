import Foundation
import Testing

@testable import YplayerKit

private let videoID = "dQw4w9WgXcQ"

// Cases from the `ytid` tests in crates/yplayer/src/ytid.rs.
@Test(arguments: [
    "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
    "http://youtube.com/watch?v=dQw4w9WgXcQ",
    "youtube.com/watch?v=dQw4w9WgXcQ",
    "www.youtube.com/watch?v=dQw4w9WgXcQ",
    "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
    "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
    "https://www.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
    "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s",
    "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=PLabc&index=2",
    "https://www.youtube.com/watch?list=PLabc&v=dQw4w9WgXcQ",
    "https://youtu.be/dQw4w9WgXcQ",
    "youtu.be/dQw4w9WgXcQ",
    "https://youtu.be/dQw4w9WgXcQ?si=x",
    "https://www.youtube.com/shorts/dQw4w9WgXcQ",
    "https://youtube.com/shorts/dQw4w9WgXcQ?feature=share",
    "https://www.youtube.com/live/dQw4w9WgXcQ",
    "https://www.youtube.com/embed/dQw4w9WgXcQ",
    "dQw4w9WgXcQ",
    "  https://youtu.be/dQw4w9WgXcQ  ",
])
func youTubeURLAcceptsAllSupportedForms(_ input: String) {
    #expect(YouTubeURL.videoID(from: input) == .success(videoID))
}

@Test func youTubeURLAcceptsBareIDWithDashAndUnderscore() {
    #expect(YouTubeURL.videoID(from: "a-_B9cD8eF7") == .success("a-_B9cD8eF7"))
}

@Test(arguments: [
    "https://www.youtube.com/playlist?list=PLabc",
    "youtube.com/playlist?list=PLabc",
    "https://www.youtube.com/watch?list=PLabc",
    "https://music.youtube.com/watch?list=PLabc&index=1",
])
func youTubeURLRejectsPlaylistOnly(_ input: String) {
    #expect(YouTubeURL.videoID(from: input) == .failure(.playlistOnly))
}

@Test(arguments: [
    "https://vimeo.com/123",
    "vimeo.com/123",
    "https://notyoutube.com/watch?v=dQw4w9WgXcQ",
])
func youTubeURLRejectsOtherHosts(_ input: String) {
    #expect(YouTubeURL.videoID(from: input) == .failure(.notYouTube))
}

@Test(arguments: [
    "https://www.youtube.com/watch?v=short",
    "https://www.youtube.com/watch?v=dQw4w9WgXcQx",
    "https://youtu.be/short",
])
func youTubeURLRejectsBadVideoID(_ input: String) {
    #expect(YouTubeURL.videoID(from: input) == .failure(.noVideoID))
}

private func album(_ id: Int64, _ name: String, lastUsedAt: Int64?) -> Album {
    Album(id: id, name: name, trackIDs: [], createdAt: 0, lastUsedAt: lastUsedAt)
}

@Test func orbTargetsRecentFirstThenNeverUsedByName() {
    let targets = OrbTargets.make(albums: [
        album(1, "zeta", lastUsedAt: nil),
        album(2, "Old", lastUsedAt: 100),
        album(3, "alpha", lastUsedAt: nil),
        album(4, "New", lastUsedAt: 300),
    ])
    #expect(
        targets.bubbles == [
            .album(id: 4, name: "New"), .album(id: 2, name: "Old"), .album(id: 3, name: "alpha"),
            .album(id: 1, name: "zeta"), .newAlbum,
        ])
    #expect(targets.center == .album(id: 4, name: "New"))
}

@Test func orbTargetsCapBubblesAndKeepNewAlbumLast() {
    let albums = (1...8).map { album(Int64($0), "A\($0)", lastUsedAt: Int64($0)) }
    #expect(
        OrbTargets.make(albums: albums).bubbles == [
            .album(id: 8, name: "A8"), .album(id: 7, name: "A7"), .album(id: 6, name: "A6"),
            .album(id: 5, name: "A5"), .album(id: 4, name: "A4"), .newAlbum,
        ])
    #expect(
        OrbTargets.make(albums: albums, maxBubbles: 2).bubbles == [
            .album(id: 8, name: "A8"), .album(id: 7, name: "A7"), .newAlbum,
        ])
    #expect(OrbTargets.make(albums: albums).center == .album(id: 8, name: "A8"))
}

@Test func orbTargetsCenterIsInboxWithoutUsedAlbum() {
    let empty = OrbTargets.make(albums: [])
    #expect(empty.bubbles == [.newAlbum])
    #expect(empty.center == .inbox)
    let neverUsed = OrbTargets.make(albums: [album(1, "a", lastUsedAt: nil)])
    #expect(neverUsed.bubbles == [.album(id: 1, name: "a"), .newAlbum])
    #expect(neverUsed.center == .inbox)
}

@Test(arguments: 1...6)
func orbLayoutEachBubbleCenterHitsItsIndex(_ count: Int) {
    let layout = OrbLayout(targetCount: count)
    #expect(layout.bubbleCenters.count == count)
    for (index, center) in layout.bubbleCenters.enumerated() {
        #expect(layout.target(at: center) == index)
    }
}

@Test func orbLayoutCoreAndOutsideHitNoBubble() {
    let layout = OrbLayout(targetCount: 6)
    #expect(layout.target(at: layout.coreCenter) == nil)
    #expect(layout.coreContains(layout.coreCenter))
    let far = CGPoint(x: layout.coreCenter.x + 500, y: layout.coreCenter.y + 500)
    #expect(layout.target(at: far) == nil)
    #expect(!layout.coreContains(far))
}

@Test(arguments: 1...6)
func orbLayoutBubblesFanLeftOnTheArc(_ count: Int) {
    let layout = OrbLayout(targetCount: count)
    for center in layout.bubbleCenters {
        #expect(center.x < layout.coreCenter.x)
        let distance = hypot(center.x - layout.coreCenter.x, center.y - layout.coreCenter.y)
        #expect(abs(distance - layout.radius) < 0.001)
    }
    if count > 1, let first = layout.bubbleCenters.first, let last = layout.bubbleCenters.last {
        #expect(first.y < layout.coreCenter.y)
        #expect(last.y > layout.coreCenter.y)
    }
}

@Test(arguments: 0...6)
func orbLayoutSizeContainsAllBubbles(_ count: Int) {
    let layout = OrbLayout(targetCount: count)
    func contains(_ center: CGPoint, radius: CGFloat) -> Bool {
        center.x - radius >= -0.001 && center.y - radius >= -0.001
            && center.x + radius <= layout.size.width + 0.001
            && center.y + radius <= layout.size.height + 0.001
    }
    for center in layout.bubbleCenters {
        #expect(contains(center, radius: layout.bubble / 2))
    }
    #expect(contains(layout.coreCenter, radius: layout.core / 2))
}

@Test(arguments: 1...6)
func orbLayoutBubblesNeverOverlapEachOtherOrTheCore(_ count: Int) {
    let layout = OrbLayout(targetCount: count)
    let centers = layout.bubbleCenters
    for (a, b) in zip(centers, centers.dropFirst()) {
        #expect(hypot(a.x - b.x, a.y - b.y) >= layout.bubble + layout.gap - 0.001)
    }
    for center in centers {
        let distance = hypot(center.x - layout.coreCenter.x, center.y - layout.coreCenter.y)
        #expect(distance >= layout.core / 2 + layout.bubble / 2 + layout.gap - 0.001)
    }
}

@Test func dropPayloadPicksFirstYouTubeURLAndTrims() {
    #expect(
        DropPayload.url(from: [
            "not a link",
            "https://vimeo.com/123",
            "  https://youtu.be/dQw4w9WgXcQ  \nsecond line",
            "https://www.youtube.com/watch?v=a-_B9cD8eF7",
        ]) == "https://youtu.be/dQw4w9WgXcQ")
    #expect(
        DropPayload.url(from: ["https://www.youtube.com/playlist?list=PLabc", "vimeo.com/123"])
            == nil)
    #expect(DropPayload.url(from: []) == nil)
}
