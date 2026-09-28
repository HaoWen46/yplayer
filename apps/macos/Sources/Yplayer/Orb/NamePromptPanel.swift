import AppKit
import SwiftUI

/// The name for a drop on "+ New…": a small key-capable borderless panel beside the orb that
/// activates the app. Return creates, Esc cancels; `completion` gets the name (nil when
/// cancelled) exactly once, after the panel is ordered out.
final class NamePromptPanel: NSPanel {
    private var completion: (@MainActor (String?) -> Void)?

    init(completion: @escaping @MainActor (String?) -> Void) {
        self.completion = completion
        super.init(
            contentRect: .zero, styleMask: [.borderless], backing: .buffered, defer: false)
        level = .statusBar
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]
        hidesOnDeactivate = false
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        isReleasedWhenClosed = false
        contentView = NSHostingView(
            rootView: NamePromptView(
                create: { [weak self] name in self?.finish(name) },
                cancel: { [weak self] in self?.finish(nil) }
            )
            .padding(8))
    }

    override var canBecomeKey: Bool { true }

    /// Shows the panel to the left of `anchor` (the orb's core, screen coordinates) and makes it
    /// key in the activated app.
    func show(beside anchor: NSRect) {
        let size = contentView?.fittingSize ?? .zero
        setFrame(
            NSRect(
                x: anchor.minX - 4 - size.width, y: anchor.midY - size.height / 2,
                width: size.width, height: size.height),
            display: false)
        NSApp.activate()
        makeKeyAndOrderFront(nil)
    }

    private func finish(_ name: String?) {
        guard let completion else { return }
        self.completion = nil
        orderOut(nil)
        completion(name)
    }
}

/// The prompt's glass card: "New album", a name field prefilled "New Album" and selected,
/// Cancel (Esc) and Create (Return).
struct NamePromptView: View {
    var create: @MainActor (String) -> Void
    var cancel: @MainActor () -> Void
    @ViewState private var name = "New Album"
    @ViewState private var selection: TextSelection?
    @FocusState private var focused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Label("New album", systemImage: "music.note.list")
                .font(.headline)
            TextField("Album name", text: $name, selection: $selection)
                .textFieldStyle(.roundedBorder)
                .focused($focused)
                .onSubmit { create(name) }
            HStack(spacing: 8) {
                Spacer(minLength: 0)
                Button("Cancel") { cancel() }
                    .keyboardShortcut(.cancelAction)
                    .buttonStyle(.glass)
                Button("Create") { create(name) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.glassProminent)
            }
        }
        .padding(16)
        .frame(width: 240)
        .glassEffect(.regular, in: .rect(cornerRadius: 20))
        .onAppear {
            focused = true
            selection = TextSelection(range: name.startIndex..<name.endIndex)
        }
    }
}
