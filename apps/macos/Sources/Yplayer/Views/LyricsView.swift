import SwiftUI
import YplayerKit

/// The current track's lyrics, fetched when the track changes. Synced lines follow the local
/// position clock through a `TimelineView` that exists only while this view is shown and the
/// player is playing (two ticks per second); the list redraws only when the active line changes.
struct LyricsView: View {
    let model: AppModel
    @Environment(\.lyricsFixture) private var fixture
    @ViewState private var phase = Phase.loading

    private enum Phase: Equatable {
        case loading
        case loaded(LyricsResult)
        case failed
    }

    var body: some View {
        let trackID = model.store.player?.trackID
        content
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .task(id: trackID) {
                if let fixture {
                    phase = .loaded(fixture)
                    return
                }
                phase = .loading
                guard let trackID else { return }
                let result = await model.lyrics(for: trackID)
                guard !Task.isCancelled else { return }
                phase = result.map(Phase.loaded) ?? .failed
            }
    }

    @ViewBuilder private var content: some View {
        switch phase {
        case .loading:
            ProgressView()
                .controlSize(.small)
        case .failed:
            ContentUnavailableView("Lyrics unavailable", systemImage: "exclamationmark.bubble")
        case .loaded(.missing):
            ContentUnavailableView("No lyrics found", systemImage: "quote.bubble")
        case .loaded(.lines(synced: true, let lines)):
            synced(lines)
        case .loaded(.lines(synced: false, let lines)):
            ScrollView {
                Text(lines.map(\.text).joined(separator: "\n"))
                    .font(.title3.weight(.semibold))
                    .lineSpacing(8)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
                    .padding(.vertical, 24)
                    .padding(.horizontal, 8)
            }
            .scrollIndicators(.never)
            .mask(fade)
        }
    }

    private func synced(_ lines: [LyricLine]) -> some View {
        ScrollViewReader { proxy in
            ScrollView {
                if let player = model.store.player, player.state == .playing {
                    TimelineView(.periodic(from: .now, by: 0.5)) { context in
                        following(
                            lines, at: PositionClock.position(player, now: context.date), proxy)
                    }
                } else {
                    let player = model.store.player
                    following(
                        lines, at: player.map { PositionClock.position($0, now: .now) }, proxy)
                }
            }
            .scrollIndicators(.never)
            .mask(fade)
        }
    }

    /// The lines with the one at `position` highlighted and scrolled to the center.
    private func following(_ lines: [LyricLine], at position: Double?, _ proxy: ScrollViewProxy)
        -> some View
    {
        let active = position.flatMap { LyricsTimeline.index(lines, at: $0) }
        return LyricLines(lines: lines, active: active)
            .equatable()
            .onChange(of: active, initial: true) { _, active in
                guard let active else { return }
                withAnimation(.smooth(duration: 0.45)) {
                    proxy.scrollTo(active, anchor: .center)
                }
            }
    }

    /// Fades lines out at the top and bottom edges.
    private var fade: some View {
        LinearGradient(
            stops: [
                .init(color: .clear, location: 0), .init(color: .black, location: 0.12),
                .init(color: .black, location: 0.88), .init(color: .clear, location: 1),
            ], startPoint: .top, endPoint: .bottom)
    }
}

/// Synced lines with `active` highlighted; each line's id is its index.
private struct LyricLines: View, Equatable {
    let lines: [LyricLine]
    let active: Int?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(lines.indices, id: \.self) { index in
                Text(lines[index].text.isEmpty ? "♪" : lines[index].text)
                    .font(.title3.weight(.bold))
                    .foregroundStyle(index == active ? .primary : .tertiary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .id(index)
            }
        }
        .padding(.vertical, 60)
        .padding(.horizontal, 8)
        .animation(.smooth(duration: 0.3), value: active)
    }
}

private struct LyricsFixtureKey: EnvironmentKey {
    static let defaultValue: LyricsResult? = nil
}

extension EnvironmentValues {
    /// Snapshots only: lyrics shown from the start without fetching (set by `SnapshotRenderer`).
    var lyricsFixture: LyricsResult? {
        get { self[LyricsFixtureKey.self] }
        set { self[LyricsFixtureKey.self] = newValue }
    }
}
