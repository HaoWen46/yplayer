import Foundation
import Observation
import Testing

@testable import YplayerKit

private func track(
    _ id: String, _ title: String, addedAt: Int64?, uploader: String? = nil
) -> Track {
    Track(
        id: id, title: title, uploader: uploader, duration: 200, webpageURL: nil, audioPath: nil,
        format: nil, fileSize: nil, addedAt: addedAt, lastPlayed: nil, state: .complete,
        thumbPath: nil)
}

private func player(
    _ state: PlayState, _ trackID: String?, position: Double, atMs: Int64,
    duration: Double? = 200
) -> PlayerState {
    PlayerState(
        state: state, trackID: trackID, context: .library, position: position, atMs: atMs,
        duration: duration, volume: 80, loopMode: .all)
}

private func date(ms: Int64) -> Date {
    Date(timeIntervalSince1970: Double(ms) / 1000)
}

@MainActor
private final class ChangeCounter {
    var count = 0
}

/// Applies `update` and reports whether observers of `store.player` saw a change.
@MainActor
private func playerChanged(_ store: LibraryStore, by update: ClientUpdate) -> Bool {
    let counter = ChangeCounter()
    withObservationTracking {
        _ = store.player
    } onChange: {
        MainActor.assumeIsolated { counter.count += 1 }
    }
    store.apply(update)
    return counter.count > 0
}

@MainActor
private func sampleStore() -> LibraryStore {
    let store = LibraryStore()
    store.load(
        LibrarySnapshot(
            tracks: [
                track("a", "track 2", addedAt: 100),
                track("b", "track 10", addedAt: 100),
                track("c", "oldest", addedAt: 50),
                track("d", "newest", addedAt: 300),
            ],
            albums: [
                Album(id: 2, name: "zeta", trackIDs: ["c", "a"], createdAt: 1, lastUsedAt: nil),
                Album(id: 1, name: "Alpha", trackIDs: ["a", "b"], createdAt: 2, lastUsedAt: nil),
            ],
            libraryVersion: 7))
    return store
}

@MainActor
@Test func loadSnapshotOrders() {
    let store = sampleStore()
    #expect(store.libraryOrder == ["d", "a", "b", "c"])
    #expect(store.albums.map(\.name) == ["Alpha", "zeta"])
    #expect(store.libraryVersion == 7)
    #expect(store.tracks.count == 4)
    #expect(store.tracks(in: store.albums[1]).map(\.id) == ["c", "a"])
    #expect(store.album(id: 2)?.name == "zeta")
}

@MainActor
@Test func upsertNewAndUpdatedTrackRepositions() {
    let store = sampleStore()
    store.apply(.event(.trackUpsert(track("e", "brand new", addedAt: 400))))
    #expect(store.libraryOrder == ["e", "d", "a", "b", "c"])

    store.apply(.event(.trackUpsert(track("c", "oldest", addedAt: 500))))
    #expect(store.libraryOrder == ["c", "e", "d", "a", "b"])

    store.apply(.event(.trackUpsert(track("b", "track 1", addedAt: 100))))
    #expect(store.libraryOrder == ["c", "e", "d", "b", "a"])
    #expect(store.tracks["b"]?.title == "track 1")
    #expect(store.search("track 1").map(\.id) == ["b"])
}

@MainActor
@Test func removedTrackDisappearsFromAlbumsAndDownloads() {
    let store = sampleStore()
    store.apply(
        .event(
            .download(
                DownloadEvent(trackID: "a", phase: .downloading, bytes: 1, total: 2, error: nil)))
    )
    #expect(store.downloads["a"] != nil)

    store.apply(.event(.trackRemoved("a")))
    #expect(store.tracks["a"] == nil)
    #expect(store.libraryOrder == ["d", "b", "c"])
    #expect(store.albums.map(\.trackIDs) == [["b"], ["c"]])
    #expect(store.downloads["a"] == nil)
    #expect(store.search("track 2").isEmpty)
}

@MainActor
@Test func duplicatePlayerEventsCoalescedWhileRealSeekApplies() {
    let store = sampleStore()
    let t0: Int64 = 1_700_000_000_000
    store.apply(.event(.player(player(.paused, "a", position: 30, atMs: t0 - 5_000))))

    // One change in the service emits 3-4 near-identical events.
    let burst = [
        player(.playing, "b", position: 0, atMs: t0),
        player(.playing, "b", position: 0, atMs: t0 + 15),
        player(.playing, "b", position: 0.05, atMs: t0 + 60),
        player(.playing, "b", position: 0.1, atMs: t0 + 110),
    ]
    let changes = burst.filter { playerChanged(store, by: .event(.player($0))) }.count
    #expect(changes == 1)
    #expect(store.player == burst[0])

    let seek = player(.playing, "b", position: 90, atMs: t0 + 2_000)
    #expect(playerChanged(store, by: .event(.player(seek))))
    #expect(store.player == seek)

    let drift = player(.playing, "b", position: 91.5, atMs: t0 + 3_000)
    #expect(!playerChanged(store, by: .event(.player(drift))))

    var louder = drift
    louder.volume = 60
    #expect(playerChanged(store, by: .event(.player(louder))))
    #expect(store.currentTrack?.id == "b")
}

@MainActor
@Test func downloadProgressFraction() {
    let store = sampleStore()
    store.apply(
        .event(
            .download(
                DownloadEvent(trackID: "e", phase: .fetching, bytes: nil, total: nil, error: nil)))
    )
    #expect(store.downloads["e"] == DownloadProgress(phase: .fetching, fraction: nil))

    store.apply(
        .event(
            .download(
                DownloadEvent(trackID: "e", phase: .downloading, bytes: 40, total: 100, error: nil))
        )
    )
    #expect(store.downloads["e"] == DownloadProgress(phase: .downloading, fraction: 0.4))

    store.apply(
        .event(
            .download(DownloadEvent(trackID: "e", phase: .done, bytes: 100, total: 100, error: nil))
        )
    )
    #expect(store.downloads["e"] == nil)
}

@MainActor
@Test func toastAppends() {
    let store = LibraryStore()
    store.apply(.event(.toast(Toast(severity: .info, message: "one"))))
    store.apply(.event(.toast(Toast(severity: .error, message: "two"))))
    #expect(store.toasts.map(\.message) == ["one", "two"])
    #expect(store.toasts.map(\.severity) == [.info, .error])
    #expect(store.toasts[0].id != store.toasts[1].id)
}

@MainActor
@Test func resyncSetsFlagAndLoadClearsIt() {
    let store = sampleStore()
    #expect(!store.needsResync)
    store.apply(.event(.resync))
    #expect(store.needsResync)
    store.load(LibrarySnapshot(tracks: [], albums: [], libraryVersion: 8))
    #expect(!store.needsResync)
    #expect(store.libraryOrder.isEmpty)
}

@Test func positionClockClamps() {
    let t0: Int64 = 1_700_000_000_000
    let playing = player(.playing, "a", position: 10, atMs: t0, duration: 20)
    #expect(PositionClock.position(playing, now: date(ms: t0 + 2_500)) == 12.5)
    #expect(PositionClock.position(playing, now: date(ms: t0 + 60_000)) == 20)
    #expect(PositionClock.position(playing, now: date(ms: t0 - 30_000)) == 0)

    let paused = player(.paused, "a", position: 10, atMs: t0, duration: 20)
    #expect(PositionClock.position(paused, now: date(ms: t0 + 60_000)) == 10)
}

@MainActor
@Test func searchNormalizerFoldsKanaWidthAndDiacritics() {
    let store = LibraryStore()
    store.load(
        LibrarySnapshot(
            tracks: [
                track("h", "ハムカツ", addedAt: 3),
                track("z", "秒針を噛む", addedAt: 2, uploader: "ＺＵＴＯＭＡＹＯ"),
                track("b", "Halo", addedAt: 1, uploader: "Beyoncé"),
            ],
            albums: [], libraryVersion: 1))
    #expect(store.search("はむ").map(\.id) == ["h"])
    #expect(store.search("zutomayo").map(\.id) == ["z"])
    #expect(store.search("beyonce").map(\.id) == ["b"])
    #expect(store.search("").isEmpty)
    #expect(SearchNormalizer.fold("はむ") == SearchNormalizer.fold("ハム"))
}

@MainActor
@Test func searchOverFiveThousandTracksIsFast() {
    let store = LibraryStore()
    let tracks = (0..<5_000).map { i in
        track(
            String(format: "id%05d", i), "ずっと真夜中でいいのに。『秒針を噛む』MV \(i)",
            addedAt: Int64(i), uploader: "ZUTOMAYO \(i % 97)")
    }
    store.load(LibrarySnapshot(tracks: tracks, albums: [], libraryVersion: 1))

    let clock = ContinuousClock()
    var results: [Track] = []
    let elapsed = clock.measure {
        results = store.search("秒針")
    }
    print("search over 5,000 tracks: \(elapsed)")
    #expect(results.count == 5_000)
    #expect(elapsed < .milliseconds(20))
}

/// `count` tracks with distinct `addedAt`, mixed CJK and Latin titles, one in six by ZUTOMAYO.
private func largeLibrary(_ count: Int) -> [Track] {
    let words = ["秒針を噛む", "お勉強しといてよ", "正しくなれない", "Night", "Blue", "Remix", "Live", "はむ"]
    let uploaders = ["ずっと真夜中でいいのに。 ZUTOMAYO", "YOASOBI", "Ado", "Vaundy", "King Gnu", "米津玄師"]
    return (0..<count).map { i in
        track(
            String(format: "id%06d", i),
            "\(words[i % words.count]) \(words[(i / words.count) % words.count]) \(i)",
            addedAt: 1_700_000_000 + Int64(i), uploader: uploaders[i % uploaders.count])
    }
}

@MainActor
@Test func perKeystrokeSearchOverTwentyThousandTracksIsFast() {
    let store = LibraryStore()
    store.load(LibrarySnapshot(tracks: largeLibrary(20_000), albums: [], libraryVersion: 1))

    let clock = ContinuousClock()
    for query in ["zutomayo", "ずっと真夜中"] {
        var slowest: Duration = .zero
        var typed = ""
        var results: [Track] = []
        for character in query {
            typed.append(character)
            let elapsed = clock.measure {
                results = store.search(typed)
            }
            slowest = max(slowest, elapsed)
        }
        print("per-keystroke search over 20,000 tracks (\(query)): slowest \(slowest)")
        let fresh = LibraryStore()
        fresh.load(LibrarySnapshot(tracks: largeLibrary(20_000), albums: [], libraryVersion: 1))
        #expect(results.map(\.id) == fresh.search(query).map(\.id))
        #expect(results.count == 3_334)
        #expect(slowest < .milliseconds(20))
    }
}

@MainActor
@Test func loadAndUpsertOverTwentyThousandTracksAreFast() {
    let tracks = largeLibrary(20_000)
    let store = LibraryStore()
    let clock = ContinuousClock()
    let load = clock.measure {
        store.load(LibrarySnapshot(tracks: tracks, albums: [], libraryVersion: 1))
    }

    var upserts: Duration = .zero
    for k in 0..<200 {
        var upserted: Track
        if k.isMultiple(of: 2) {
            upserted = track(
                "new\(k)", "Night \(k)", addedAt: 1_700_000_000 + Int64((k * 7_919) % 20_000))
        } else {
            upserted = tracks[(k * 197) % tracks.count]
            upserted.title = "renamed \(k)"
        }
        upserts += clock.measure {
            store.apply(.event(.trackUpsert(upserted)))
        }
    }
    let upsert = upserts / 200
    print("20,000 tracks: load \(load), upsert \(upsert)")

    let fresh = LibraryStore()
    fresh.load(
        LibrarySnapshot(tracks: Array(store.tracks.values), albums: [], libraryVersion: 1))
    #expect(store.libraryOrder == fresh.libraryOrder)
    #expect(store.search("renamed 1").map(\.id) == fresh.search("renamed 1").map(\.id))
    #expect(load < .milliseconds(450))
    #expect(upsert < .microseconds(1_500))
}
