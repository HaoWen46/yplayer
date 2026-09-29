import SwiftUI
import YplayerKit

/// One Up Next row: its section, index in that section and track (a track can be listed twice).
private struct QueueEntry: Hashable {
    let section: QueueSection
    let index: Int
    let trackID: String
}

/// Up Next in the library's place below the now-playing card: "Playing Next" (the play-next
/// entries; Clear, drag to reorder) above "Up Next from <album | Library>" (the context tracks
/// that follow, in play order). Double-click or Return plays a row now; the context menu plays or
/// removes it; swipe left and ⌫ remove it without asking (nothing is deleted). Failed tracks are
/// dimmed; in `Single` a note says the song repeats.
struct UpNextView: View {
    let model: AppModel
    @ViewState private var selection: QueueEntry?

    var body: some View {
        let queue = model.store.queue
        VStack(spacing: 8) {
            if model.store.player?.loopMode == .single {
                Label("Repeating this song", systemImage: "repeat.1")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .glassEffect(.regular, in: .capsule)
            }
            if queue.next.isEmpty && queue.upcoming.isEmpty {
                ContentUnavailableView("Nothing is up next.", systemImage: "list.bullet")
                    .frame(maxHeight: .infinity)
            } else {
                list(queue)
            }
        }
    }

    private func list(_ queue: QueueState) -> some View {
        // Section titles are plain rows: a `Section` header floats with a heavy rule under it.
        List(selection: $selection) {
            if !queue.next.isEmpty {
                header("Playing Next") {
                    Button("Clear") {
                        Task { await model.clearQueue() }
                    }
                    .buttonStyle(.glass)
                    .controlSize(.small)
                }
                ForEach(entries(.next, queue.next), id: \.self, content: row)
                    .onMove { offsets, destination in
                        move(offsets, destination, queue.next)
                    }
            }
            if !queue.upcoming.isEmpty {
                header(upcomingTitle(queue.context)) { EmptyView() }
                    .padding(.top, queue.next.isEmpty ? 0 : 10)
                ForEach(entries(.upcoming, queue.upcoming), id: \.self, content: row)
                if queue.more {
                    Text("and more…")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 6)
                        .listRowSeparator(.hidden)
                }
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
        .contextMenu(forSelectionType: QueueEntry.self) { entries in
            if let entry = entries.first {
                Button("Play Now") { jump(entry) }
                    .disabled(failed(entry))
                Button("Remove from Up Next") { remove(entry) }
            }
        } primaryAction: { entries in
            if let entry = entries.first { jump(entry) }
        }
        .onKeyPress(.return) {
            guard let entry = selection else { return .ignored }
            jump(entry)
            return .handled
        }
        .onDeleteCommand {
            if let entry = selection { remove(entry) }
        }
    }

    private func header(_ title: String, @ViewBuilder trailing: () -> some View) -> some View {
        HStack(spacing: 8) {
            Text(title)
                .font(.headline)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Spacer(minLength: 0)
            trailing()
        }
        .frame(minHeight: 24)
        .padding(.horizontal, 6)
        .listRowInsets(EdgeInsets(top: 2, leading: 0, bottom: 2, trailing: 0))
        .listRowSeparator(.hidden)
    }

    private func row(_ entry: QueueEntry) -> some View {
        TrackRow(model: model, trackID: entry.trackID)
            .opacity(failed(entry) ? 0.45 : 1)
            .listRowInsets(EdgeInsets(top: 1, leading: 0, bottom: 1, trailing: 0))
            .listRowSeparatorTint(Color.primary.opacity(0.06))
            .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                Button {
                    remove(entry)
                } label: {
                    Label("Remove", systemImage: "minus.circle")
                }
                .tint(.red)
            }
    }

    private func entries(_ section: QueueSection, _ ids: [String]) -> [QueueEntry] {
        ids.enumerated().map { QueueEntry(section: section, index: $0.offset, trackID: $0.element) }
    }

    /// "Up Next from <album name>" or "Up Next from Library".
    private func upcomingTitle(_ context: ContextRef?) -> String {
        switch context {
        case .album(let id):
            model.store.album(id: id).map { "Up Next from \($0.name)" } ?? "Up Next"
        case .library:
            "Up Next from Library"
        case nil:
            "Up Next"
        }
    }

    private func failed(_ entry: QueueEntry) -> Bool {
        model.store.tracks[entry.trackID]?.state == .failed
    }

    private func jump(_ entry: QueueEntry) {
        guard !failed(entry) else { return }
        Task { await model.jumpInQueue(entry.section, entry.index, entry.trackID) }
    }

    private func remove(_ entry: QueueEntry) {
        Task { await model.removeFromQueue(entry.section, entry.index, entry.trackID) }
    }

    /// `onMove`'s destination is an index before the removal; `queue.move` takes the final one.
    private func move(_ offsets: IndexSet, _ destination: Int, _ next: [String]) {
        guard let from = offsets.first else { return }
        let to = destination > from ? destination - 1 : destination
        guard to != from else { return }
        Task { await model.moveInQueue(from: from, to: to, next[from]) }
    }
}
