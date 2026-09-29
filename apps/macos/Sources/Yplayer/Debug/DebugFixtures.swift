import Foundation
import YplayerKit

/// The fixture library, player and lyrics behind `--snapshot-dir` (no socket).
@MainActor
enum DebugFixtures {
    static let zutomayo = "ずっと真夜中でいいのに。 ZUTOMAYO"

    static let tracks: [Track] = [
        track(1, "ずっと真夜中でいいのに。『秒針を噛む』MV", zutomayo, 231, size: 4_402_117),
        track(2, "ずっと真夜中でいいのに。『お勉強しといてよ』MV", zutomayo, 229, size: 4_371_560),
        track(3, "ずっと真夜中でいいのに。『正しくなれない』MV", zutomayo, 247, size: 4_712_004),
        track(4, "ずっと真夜中でいいのに。『あいつら全員同窓会』MV", zutomayo, 246, size: 4_690_338),
        track(5, "ヨルシカ - 言って。(Music Video)", "ヨルシカ / n-buna Official", 258, size: 4_925_871),
        track(
            6, "아이유 (IU) - 밤편지 (Through the Night) Live Clip", "이지금 [IU Official]", 254,
            size: 4_848_902),
        track(
            7,
            "Nujabes — Aruarian Dance (Samurai Champloo Original Soundtrack, Remastered Extended "
                + "Edition with Bonus Interlude)",
            "Hydeout Productions", 238, size: nil, state: .downloading),
        track(
            8, "YOASOBI「夜に駆ける」Official Music Video", "Ayase / YOASOBI", 261, size: nil,
            state: .failed),
    ]

    static let albums: [Album] = [
        Album(
            id: 1, name: "ずとまよ",
            trackIDs: ["fixture-001", "fixture-002", "fixture-003", "fixture-004"],
            createdAt: 1_790_000_000, lastUsedAt: 1_790_600_000),
        Album(
            id: 2, name: "Night Drive",
            trackIDs: ["fixture-005", "fixture-006", "fixture-001", "fixture-007"],
            createdAt: 1_790_100_000, lastUsedAt: 1_790_500_000),
    ]

    /// Synced lyrics for the playing track; 1:02 falls on the third line.
    static let lyrics = LyricsResult.lines(
        synced: true,
        [
            LyricLine(tMs: 52_000, text: "夜更けの窓に 映る灯り"),
            LyricLine(tMs: 57_500, text: "数えきれない 秒の欠片"),
            LyricLine(tMs: 62_000, text: "まだ眠れない 針の音だけ"),
            LyricLine(tMs: 67_000, text: "明日のことは 後でいいから"),
            LyricLine(tMs: 72_500, text: "静かなままで 歌っていて"),
        ])

    /// Playing the first track in the first album at 1:02 of 3:51.
    static func player(at now: Date = .now) -> PlayerState {
        PlayerState(
            state: .playing, trackID: "fixture-001", context: .album(1), position: 62,
            atMs: Int64(now.timeIntervalSince1970 * 1000), duration: 231, volume: 70, loopMode: .all
        )
    }

    /// The fixture library, connected and playing, with track 7 downloading at 40 %.
    static func store() -> LibraryStore {
        let store = LibraryStore()
        store.apply(.connected(SubscribeResult(player: player(), libraryVersion: 1)))
        store.load(LibrarySnapshot(tracks: tracks, albums: albums, libraryVersion: 1))
        store.apply(
            .event(
                .download(
                    DownloadEvent(
                        trackID: "fixture-007", phase: .downloading, bytes: 4_000_000,
                        total: 10_000_000, error: nil))))
        return store
    }

    /// A model over `store` whose client is never started.
    static func model(_ store: LibraryStore) -> AppModel {
        AppModel(client: ServiceClient(socketPath: "/dev/null"), store: store)
    }

    /// The fixture library after the connection dropped, retrying in 5 s.
    static func disconnectedModel() -> AppModel {
        let store = store()
        store.apply(.disconnected(retryIn: .seconds(5)))
        return model(store)
    }

    /// The fixture library asking to delete the playing track.
    static func confirmDeleteModel() -> AppModel {
        let model = model(store())
        let track = tracks[0]
        let size = (track.fileSize ?? 0).formatted(.byteCount(style: .file))
        model.confirm = ConfirmRequest(
            title: "Delete “\(track.title)” from your library?",
            message: "The \(size) file moves to the Trash.", actionTitle: "Delete", action: {})
        return model
    }

    /// Connected, nothing playing, no songs.
    static func emptyModel() -> AppModel {
        let store = LibraryStore()
        let stopped = PlayerState(
            state: .stopped, trackID: nil, context: nil, position: 0, atMs: 0, duration: nil,
            volume: 100, loopMode: .none)
        store.apply(.connected(SubscribeResult(player: stopped, libraryVersion: 0)))
        store.load(LibrarySnapshot(tracks: [], albums: [], libraryVersion: 0))
        return model(store)
    }

    /// The fixture library playing track 1 with `artwork` as its cover.
    static func artworkModel(_ artwork: String?) -> AppModel {
        let store = store()
        var track = tracks[0]
        track.thumbPath = artwork
        store.apply(.event(.trackUpsert(track)))
        return model(store)
    }

    /// `artworkModel`, paused at 1:02.
    static func pausedModel(_ artwork: String?) -> AppModel {
        let model = artworkModel(artwork)
        var paused = player()
        paused.state = .paused
        model.store.apply(.event(.player(paused)))
        return model
    }

    private static func track(
        _ n: Int, _ title: String, _ uploader: String, _ duration: Int, size: Int64?,
        state: TrackState = .complete
    ) -> Track {
        let id = String(format: "fixture-%03d", n)
        return Track(
            id: id, title: title, uploader: uploader, duration: duration,
            webpageURL: "https://www.youtube.com/watch?v=\(id)",
            audioPath: state == .complete ? "/tmp/yplayer-fixtures/\(id)/audio.opus" : nil,
            format: state == .complete ? "opus" : nil, fileSize: size,
            addedAt: 1_790_600_000 - Int64(n) * 3_600, lastPlayed: nil, state: state,
            thumbPath: nil)
    }

    /// Songs: the downloading and failed tracks re-added as the newest, so they lead the list.
    static func songsModel() -> AppModel {
        let store = store()
        for var track in tracks where track.state != .complete {
            track.addedAt = 1_790_700_000
            store.apply(.event(.trackUpsert(track)))
        }
        return model(store)
    }

    /// Songs over a 5,000-track library: the fixture library plus numbered copies of its
    /// complete tracks, older than the fixture tracks.
    static func largeSongsModel() -> AppModel {
        let store = store()
        let complete = tracks.filter { $0.state == .complete }
        let more = (0..<(5_000 - tracks.count)).map { i in
            let base = complete[i % complete.count]
            return track(
                101 + i, "\(base.title) #\(i + 1)", base.uploader ?? "", base.duration ?? 0,
                size: base.fileSize)
        }
        store.load(LibrarySnapshot(tracks: tracks + more, albums: albums, libraryVersion: 1))
        return model(store)
    }

    /// A katakana "ハム" title for the search state (query "はむ" folds to it).
    static let kanaTrack = track(
        9, "とっとこハム太郎 OP『ハム太郎とっとこうた』", "ハムちゃんず", 203, size: 3_874_210)

    /// The fixture library plus `kanaTrack`.
    static func searchModel() -> AppModel {
        let store = store()
        store.apply(.event(.trackUpsert(kanaTrack)))
        return model(store)
    }

    /// Albums for the orb: five used ones (CJK, Korean, a long Latin name) and a never-used one
    /// past the five-bubble cap.
    static let orbAlbums: [Album] =
        albums + [
            Album(
                id: 3, name: "ヨルシカ", trackIDs: [], createdAt: 1_790_200_000,
                lastUsedAt: 1_790_400_000),
            Album(
                id: 4, name: "Late Night Lo-fi Study Beats", trackIDs: [], createdAt: 1_790_300_000,
                lastUsedAt: 1_790_300_000),
            Album(
                id: 5, name: "아이유", trackIDs: [], createdAt: 1_790_300_000,
                lastUsedAt: 1_790_200_000),
            Album(id: 6, name: "Workout", trackIDs: [], createdAt: 1_790_300_000, lastUsedAt: nil),
        ]

    /// The orb over `orbAlbums` in `phase`.
    static func orbState(_ phase: OrbPhase) -> OrbState {
        let state = OrbState()
        state.setTargets(OrbTargets.make(albums: orbAlbums))
        state.phase = phase
        return state
    }

    /// The toast after a new song was dropped on "ずとまよ".
    static let addedToast = AddedToast(
        album: "ずとまよ",
        result: AddResult(trackID: "fixture-001", albumID: 1, wasNew: true, wasInAlbum: false))
}
