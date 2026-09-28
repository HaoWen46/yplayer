import AppKit
import SwiftUI
import YplayerKit

enum LibraryTab: Hashable {
    case albums, songs, search
}

/// Where the library starts: tab, open album and search query (default: Songs).
struct LibraryRoute {
    var tab: LibraryTab = .songs
    var albumID: Int64?
    var query = ""
}

/// A name the user types in `LibraryTabs`' prompt card (New Album…, Rename…).
struct NamePrompt: Identifiable {
    let id = UUID()
    var title: String
    var initial: String
    var actionTitle: String
    var commit: @MainActor (String) async -> Void
}

/// The album name `AlbumsView` edits inline.
enum AlbumEdit: Equatable {
    case create
    case rename(Int64)
}

/// The library's view state, shared by the tabs through the environment.
@MainActor
@Observable
final class LibraryUI {
    var tab: LibraryTab
    /// The album shown by `AlbumDetailView` in the Albums tab.
    var albumID: Int64?
    var query: String
    /// Bumped by ⌘F; `SearchView` focuses its field on every change.
    var searchFocusRequest = 0
    var prompt: NamePrompt?
    var albumEdit: AlbumEdit?
    @ObservationIgnored private var keyMonitor: Any?

    init(_ route: LibraryRoute) {
        tab = route.tab
        albumID = route.albumID
        query = route.query
    }

    /// `PopoverView`'s Space and Esc handlers see every key before a focused text field does, so
    /// Space would toggle playback (never reaching the field or its input method) and Esc would
    /// close the popover mid-edit. While the library is shown, this monitor gives both keys to a
    /// focused field first.
    func startKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, self.handleFieldKey(event) else { return event }
            return nil
        }
    }

    func stopKeyMonitor() {
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
    }

    /// Space types into the focused field (through its input method); Esc goes to the input
    /// method while composing, else cancels the name prompt or the inline album name. Returns
    /// whether `event` was consumed.
    private func handleFieldKey(_ event: NSEvent) -> Bool {
        guard let editor = event.window?.firstResponder as? NSTextView, editor.isFieldEditor,
            event.modifierFlags.isDisjoint(with: [.command, .control, .option])
        else { return false }
        switch event.keyCode {
        case 49:  // Space
            editor.interpretKeyEvents([event])
            return true
        case 53:  // Esc
            if editor.hasMarkedText() {
                editor.interpretKeyEvents([event])
            } else if prompt != nil {
                prompt = nil
            } else if albumEdit != nil {
                albumEdit = nil
            } else {
                return false
            }
            return true
        default:
            return false
        }
    }
}

/// Segmented Albums | Songs | Search over the selected list; ⌘F selects Search and focuses its
/// field.
struct LibraryTabs: View {
    let model: AppModel
    @Environment(\.initialLibraryRoute) private var route

    var body: some View {
        LibraryContent(model: model, route: route)
    }
}

private struct LibraryContent: View {
    let model: AppModel
    @ViewState private var ui: LibraryUI

    init(model: AppModel, route: LibraryRoute) {
        self.model = model
        _ui = State(initialValue: LibraryUI(route))
    }

    var body: some View {
        VStack(spacing: 8) {
            Picker("Library", selection: $ui.tab) {
                Text("Albums").tag(LibraryTab.albums)
                Text("Songs").tag(LibraryTab.songs)
                Text("Search").tag(LibraryTab.search)
            }
            .pickerStyle(.segmented)
            .labelsHidden()
            Group {
                switch ui.tab {
                case .albums:
                    if let id = ui.albumID, let album = model.store.album(id: id) {
                        AlbumDetailView(model: model, album: album)
                    } else {
                        AlbumsView(model: model)
                    }
                case .songs:
                    SongsView(model: model)
                case .search:
                    SearchView(model: model)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .environment(ui)
        .background {
            Button("Search Library") {
                ui.tab = .search
                ui.searchFocusRequest += 1
            }
            .keyboardShortcut("f", modifiers: .command)
            .hidden()
        }
        .overlay {
            if let prompt = ui.prompt {
                ZStack {
                    Rectangle()
                        .fill(.black.opacity(0.15))
                        .onTapGesture { ui.prompt = nil }
                    NamePromptCard(prompt: prompt) { ui.prompt = nil }
                }
                .transition(.opacity)
            }
        }
        .animation(.snappy(duration: 0.2), value: ui.prompt?.id)
        .onAppear { ui.startKeyMonitor() }
        .onDisappear { ui.stopKeyMonitor() }
    }
}

/// A title, a name field and Cancel / action buttons; Return commits, Esc (`LibraryUI`'s key
/// monitor) cancels.
private struct NamePromptCard: View {
    let prompt: NamePrompt
    let dismiss: () -> Void
    @ViewState private var text: String
    @FocusState private var focused: Bool

    init(prompt: NamePrompt, dismiss: @escaping () -> Void) {
        self.prompt = prompt
        self.dismiss = dismiss
        _text = State(initialValue: prompt.initial)
    }

    private var name: String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var body: some View {
        VStack(spacing: 14) {
            Text(prompt.title)
                .font(.headline)
            TextField("Name", text: $text)
                .textFieldStyle(.roundedBorder)
                .focused($focused)
                .onSubmit(submit)
            HStack(spacing: 10) {
                Button {
                    dismiss()
                } label: {
                    Text("Cancel").frame(maxWidth: .infinity)
                }
                .buttonStyle(.glass)
                Button {
                    submit()
                } label: {
                    Text(prompt.actionTitle).frame(maxWidth: .infinity)
                }
                .buttonStyle(.glassProminent)
                .disabled(name.isEmpty)
            }
            .controlSize(.large)
        }
        .padding(18)
        .frame(width: 280)
        .glassEffect(.regular, in: .rect(cornerRadius: 20))
        .task { focused = true }
    }

    private func submit() {
        let name = name
        guard !name.isEmpty else { return }
        dismiss()
        Task { await prompt.commit(name) }
    }
}

private struct InitialLibraryRouteKey: EnvironmentKey {
    static let defaultValue = LibraryRoute()
}

extension EnvironmentValues {
    /// Where `LibraryTabs` starts (set by the `--snapshot-dir` states).
    var initialLibraryRoute: LibraryRoute {
        get { self[InitialLibraryRouteKey.self] }
        set { self[InitialLibraryRouteKey.self] = newValue }
    }
}
