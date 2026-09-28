import AppKit
import SwiftUI
import YplayerKit

/// The drop orb: one borderless, non-activating panel created at launch and kept ordered out.
/// `DragWatcher` shows it at the right edge of the cursor's screen while a URL drag is on screen;
/// `OrbDropView` blooms it and hands over the drop; the drop adds (and plays) the song, then an
/// Undo toast stays for 6 s. No timers run while it is hidden.
@MainActor
final class OrbController {
    private let model: AppModel
    private let state = OrbState()
    private let panel: NSPanel
    private let dropView: OrbDropView
    private var watcher: DragWatcher?
    private var prompt: NamePromptPanel?
    /// Hides the orb 0.3 s after the drag's mouse-up.
    private var pendingHide: Task<Void, Never>?
    /// Ends the shake message, the toast or "Removed".
    private var dismissal: Task<Void, Never>?

    init(model: AppModel, isPopoverShown: @escaping @MainActor () -> Bool, logDrags: Bool) {
        self.model = model
        panel = NSPanel(
            contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered,
            defer: true)
        panel.level = .statusBar
        panel.collectionBehavior = [
            .canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle,
        ]
        panel.hidesOnDeactivate = false
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.isReleasedWhenClosed = false
        dropView = OrbDropView(state: state)
        panel.contentView = dropView
        dropView.controller = self
        let host = OrbHostingView(
            rootView: OrbView(state: state, undo: { [weak self] in self?.undo() }))
        host.sizingOptions = []
        host.frame = dropView.bounds
        host.autoresizingMask = [.width, .height]
        dropView.addSubview(host)
        watcher = DragWatcher(
            logDrags: logDrags, isPopoverShown: isPopoverShown,
            onDrag: { [weak self] in self?.show() },
            onMouseUp: { [weak self] in self?.mouseUp() })
    }

    // MARK: Drop view

    func bloom() {
        guard state.phase == .armed else { return }
        setPhase(.bloom(hovered: nil))
    }

    func hover(_ index: Int?) {
        guard case .bloom(let hovered) = state.phase, hovered != index else { return }
        setPhase(.bloom(hovered: index))
    }

    func collapse() {
        guard case .bloom = state.phase, !state.dropInProgress else { return }
        setPhase(.armed)
    }

    /// An invalid drop: shake beside `message` for 2 s, then hide.
    func reject(_ message: String) {
        setPhase(.shake(message))
        dismiss(after: .seconds(2))
    }

    func drop(url: String, target: OrbTarget) {
        state.dropInProgress = true
        if target == .newAlbum {
            setPhase(.armed)
            askName(url: url)
        } else {
            Task { await add(url: url, target: target) }
        }
    }

    // MARK: Drag watcher

    private func show() {
        guard !state.dropInProgress else { return }
        pendingHide?.cancel()
        dismissal?.cancel()
        state.setTargets(OrbTargets.make(albums: model.store.albums))
        place()
        panel.orderFrontRegardless()
        setPhase(.armed)
    }

    /// Hides 0.3 s after the mouse-up unless a drop is in progress or a message is showing.
    private func mouseUp() {
        pendingHide?.cancel()
        pendingHide = Task { [weak self] in
            do {
                try await Task.sleep(for: .seconds(0.3))
            } catch {
                return
            }
            guard let self, !self.state.dropInProgress else { return }
            switch self.state.phase {
            case .shake, .toast: return
            case .hidden, .armed, .bloom: self.hide()
            }
        }
    }

    // MARK: Drop handling

    private func add(url: String, target: OrbTarget) async {
        let result = await model.add(url: url, target: target)
        state.dropInProgress = false
        guard let result else {
            hide()
            return
        }
        let album = model.store.album(id: result.albumID)?.name ?? state.name(of: target)
        setPhase(.toast(AddedToast(album: album, result: result)))
        dismiss(after: .seconds(6))
    }

    private func undo() {
        guard case .toast(let toast) = state.phase, !toast.removed else { return }
        dismissal?.cancel()
        Task {
            await model.undoAdd(toast.result)
            var removed = toast
            removed.removed = true
            setPhase(.toast(removed))
            dismiss(after: .seconds(1))
        }
    }

    private func askName(url: String) {
        let prompt = NamePromptPanel { [weak self] name in self?.named(name, url: url) }
        self.prompt = prompt
        let frame = panel.frame
        let core = state.layout.core
        prompt.show(
            beside: NSRect(
                x: frame.maxX - core, y: frame.midY - core / 2, width: core, height: core))
    }

    private func named(_ name: String?, url: String) {
        prompt = nil
        guard let name else {
            hide()
            return
        }
        Task {
            guard let album = await model.createAlbum(name) else {
                hide()
                return
            }
            await add(url: url, target: .album(id: album.id, name: album.name))
        }
    }

    // MARK: Panel

    /// Right edge, vertically centered, of the screen containing the cursor, inset 12 pt.
    private func place() {
        let mouse = NSEvent.mouseLocation
        guard
            let screen = NSScreen.screens.first(where: { NSMouseInRect(mouse, $0.frame, false) })
                ?? NSScreen.main
        else { return }
        let visible = screen.visibleFrame
        let size = state.canvasSize
        panel.setFrame(
            NSRect(
                x: visible.maxX - 12 - size.width, y: visible.midY - size.height / 2,
                width: size.width, height: size.height),
            display: false)
    }

    private func hide() {
        pendingHide?.cancel()
        dismissal?.cancel()
        state.dropInProgress = false
        setPhase(.hidden)
        panel.orderOut(nil)
        watcher?.stopPolling()
    }

    private func dismiss(after duration: Duration) {
        dismissal?.cancel()
        dismissal = Task { [weak self] in
            do {
                try await Task.sleep(for: duration)
            } catch {
                return
            }
            self?.hide()
        }
    }

    private func setPhase(_ phase: OrbPhase) {
        state.phase = phase
        dropView.needsDisplay = true
    }
}

/// Takes the first click on the toast's Undo while another app is active.
private final class OrbHostingView: NSHostingView<OrbView> {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}
