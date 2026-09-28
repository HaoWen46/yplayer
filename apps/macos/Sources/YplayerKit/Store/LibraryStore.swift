import Foundation
import Observation

/// The app's view of the service: library, player, downloads, connection and toasts.
@MainActor
@Observable
public final class LibraryStore {
    public private(set) var tracks: [String: Track] = [:]
    /// Track ids by `addedAt` descending, then title.
    public private(set) var libraryOrder: [String] = []
    /// Albums by name.
    public private(set) var albums: [Album] = []
    public private(set) var player: PlayerState?
    public private(set) var downloads: [String: DownloadProgress] = [:]
    public private(set) var connection: ConnectionStatus = .connecting
    public var toasts: [ToastItem] = []
    public private(set) var libraryVersion: UInt64 = 0
    /// Set by a `resync` event; cleared by `load`.
    public private(set) var needsResync = false

    /// Sort keys, aligned with `libraryOrder`.
    @ObservationIgnored private var sortKeys: [SortKey] = []
    /// Folded title and uploader (UTF-8) per track, aligned with `libraryOrder`, for `search`.
    @ObservationIgnored private var searchKeys: [[UInt8]] = []
    /// The previous search's folded query and matches (indices into `libraryOrder`); nil after
    /// any library change.
    @ObservationIgnored private var lastSearch: (needle: [UInt8], matches: [Int])?

    /// How long a toast is shown or kept.
    public static let toastLifetime: TimeInterval = 10
    /// The most toasts kept.
    public static let maxToasts = 3

    public init() {}

    public var currentTrack: Track? {
        player?.trackID.flatMap { tracks[$0] }
    }

    public func tracks(in album: Album) -> [Track] {
        album.trackIDs.compactMap { tracks[$0] }
    }

    public func album(id: Int64) -> Album? {
        albums.first { $0.id == id }
    }

    /// Tracks whose folded title or uploader contains the folded query, in library order. A
    /// query that extends the previous one filters the previous matches only.
    public func search(_ query: String) -> [Track] {
        let needle = Array(SearchNormalizer.fold(query).utf8)
        if needle.isEmpty { return [] }
        let matches: [Int]
        if let last = lastSearch, needle.starts(with: last.needle) {
            matches = last.matches.filter { Self.contains(searchKeys[$0], needle) }
        } else {
            matches = searchKeys.indices.filter { Self.contains(searchKeys[$0], needle) }
        }
        lastSearch = (needle, matches)
        return matches.compactMap { tracks[libraryOrder[$0]] }
    }

    public func load(_ snapshot: LibrarySnapshot) {
        tracks = Dictionary(snapshot.tracks.map { ($0.id, $0) }, uniquingKeysWith: { $1 })
        var entries = tracks.values.map { (key: SortKey($0), search: Self.searchKey($0)) }
        entries.sort { $0.key.precedes($1.key) }
        sortKeys = entries.map(\.key)
        searchKeys = entries.map(\.search)
        lastSearch = nil
        libraryOrder = sortKeys.map(\.id)
        albums = snapshot.albums.sorted(by: Self.namePrecedes)
        downloads = downloads.filter { tracks[$0.key]?.state == .downloading }
        libraryVersion = snapshot.libraryVersion
        needsResync = false
    }

    /// Appends `toast` after dropping toasts older than `toastLifetime`, unless it repeats the
    /// newest toast exactly; keeps the newest `maxToasts`.
    public func appendToast(_ toast: ToastItem) {
        var kept = toasts.filter {
            toast.createdAt.timeIntervalSince($0.createdAt) < Self.toastLifetime
        }
        if let newest = kept.last, newest.severity == toast.severity,
            newest.message == toast.message
        {
            toasts = kept
            return
        }
        kept.append(toast)
        toasts = Array(kept.suffix(Self.maxToasts))
    }

    public func apply(_ update: ClientUpdate) {
        switch update {
        case .connecting:
            connection = .connecting
        case .connected(let result):
            connection = .connected
            player = result.player
        case .disconnected(let retryIn):
            let (seconds, attoseconds) = retryIn.components
            let interval = Double(seconds) + Double(attoseconds) / 1e18
            connection = .disconnected(retryAt: Date().addingTimeInterval(interval))
        case .event(let event):
            apply(event)
        }
    }

    private func apply(_ event: Event) {
        switch event {
        case .player(let state):
            applyPlayer(state)
        case .trackUpsert(let track):
            upsert(track)
        case .trackRemoved(let id):
            remove(trackID: id)
        case .albumUpsert(let album):
            if let index = albums.firstIndex(where: { $0.id == album.id }) {
                albums[index] = album
            } else {
                albums.append(album)
            }
            albums.sort(by: Self.namePrecedes)
        case .albumRemoved(let id):
            albums.removeAll { $0.id == id }
        case .download(let download):
            switch download.phase {
            case .done, .failed, .cancelled:
                downloads[download.trackID] = nil
            case .fetching, .downloading:
                var fraction: Double?
                if let bytes = download.bytes, let total = download.total, total > 0 {
                    fraction = Double(bytes) / Double(total)
                }
                downloads[download.trackID] = DownloadProgress(
                    phase: download.phase, fraction: fraction)
            }
        case .toast(let toast):
            appendToast(ToastItem(severity: toast.severity, message: toast.message))
        case .resync:
            needsResync = true
        case .unknown:
            break
        }
    }

    /// Replaces `player` only on a real change: any field other than `position`/`atMs`, or a
    /// position more than 1 s away from the extrapolated clock.
    private func applyPlayer(_ new: PlayerState) {
        guard let old = player else {
            player = new
            return
        }
        var oldFields = old
        var newFields = new
        oldFields.position = 0
        oldFields.atMs = 0
        newFields.position = 0
        newFields.atMs = 0
        let expected = PositionClock.position(
            old, now: Date(timeIntervalSince1970: Double(new.atMs) / 1000))
        if oldFields != newFields || abs(new.position - expected) > 1 {
            player = new
        }
    }

    private func upsert(_ track: Track) {
        let old = tracks.updateValue(track, forKey: track.id)
        lastSearch = nil
        let key = SortKey(track)
        let search = Self.searchKey(track)
        guard let old else {
            let index = insertionIndex(key)
            sortKeys.insert(key, at: index)
            searchKeys.insert(search, at: index)
            libraryOrder.insert(track.id, at: index)
            return
        }
        let from = insertionIndex(SortKey(old))
        if old.addedAt == track.addedAt, old.title == track.title {
            searchKeys[from] = search
            return
        }
        sortKeys.remove(at: from)
        searchKeys.remove(at: from)
        let to = insertionIndex(key)
        sortKeys.insert(key, at: to)
        searchKeys.insert(search, at: to)
        if to != from {
            Self.move(&libraryOrder, from: from, to: to)
        }
    }

    private func remove(trackID id: String) {
        if let old = tracks.removeValue(forKey: id) {
            let index = insertionIndex(SortKey(old))
            sortKeys.remove(at: index)
            searchKeys.remove(at: index)
            libraryOrder.remove(at: index)
            lastSearch = nil
        }
        for index in albums.indices where albums[index].trackIDs.contains(id) {
            albums[index].trackIDs.removeAll { $0 == id }
        }
        downloads[id] = nil
    }

    /// The first index in `sortKeys` whose key does not precede `key` (binary search); for a
    /// key in `sortKeys`, its index.
    private func insertionIndex(_ key: SortKey) -> Int {
        var low = 0
        var high = sortKeys.count
        while low < high {
            let mid = (low + high) / 2
            if sortKeys[mid].precedes(key) {
                low = mid + 1
            } else {
                high = mid
            }
        }
        return low
    }

    private static func move(_ order: inout [String], from: Int, to: Int) {
        let id = order.remove(at: from)
        order.insert(id, at: to)
    }

    /// Whether `needle` occurs in `haystack`: `memchr` for its last byte, then `memcmp` of the
    /// bytes before it (a first-byte probe is slow on CJK text, full of the lead byte E3).
    private static func contains(_ haystack: [UInt8], _ needle: [UInt8]) -> Bool {
        let tail = needle.count - 1
        guard haystack.count > tail else { return false }
        return haystack.withUnsafeBytes { haystack in
            needle.withUnsafeBytes { needle in
                guard let base = haystack.baseAddress, let pattern = needle.baseAddress else {
                    return false
                }
                let end = base + haystack.count
                var from = base + tail
                while from < end, let found = memchr(from, Int32(needle[tail]), end - from) {
                    let hit = UnsafeRawPointer(found)
                    if memcmp(hit - tail, pattern, tail) == 0 { return true }
                    from = hit + 1
                }
                return false
            }
        }
    }

    private static func namePrecedes(_ a: Album, _ b: Album) -> Bool {
        a.name.localizedStandardCompare(b.name) == .orderedAscending
    }

    private static func searchKey(_ track: Track) -> [UInt8] {
        Array(SearchNormalizer.fold(track.title + "\n" + (track.uploader ?? "")).utf8)
    }
}

/// A track's place in `libraryOrder`: `addedAt` descending, then title (Finder order), then id.
private struct SortKey {
    let addedAt: Int64
    /// The title as a Foundation string, so comparisons do not bridge.
    let title: NSString
    let id: String

    init(_ track: Track) {
        addedAt = track.addedAt ?? .min
        title = NSString(string: track.title)
        id = track.id
    }

    func precedes(_ other: SortKey) -> Bool {
        if addedAt != other.addedAt { return addedAt > other.addedAt }
        switch title.localizedStandardCompare(other.title as String) {
        case .orderedAscending: return true
        case .orderedDescending: return false
        case .orderedSame: return id < other.id
        }
    }
}
