import SwiftUI
import YplayerKit

/// The popover's content: connection banner, now playing and library, with the confirm and
/// toast overlays. Space toggles play/pause; Esc closes the confirm overlay or else the popover.
struct PopoverView: View {
    let model: AppModel
    @Environment(\.closePopover) private var closePopover
    @FocusState private var focused: Bool

    var body: some View {
        VStack(spacing: 12) {
            if model.store.connection != .connected {
                ConnectionBanner(model: model)
                    .transition(.move(edge: .top).combined(with: .opacity))
            }
            NowPlayingCard(model: model)
            if model.showsLyrics && model.store.currentTrack != nil {
                LyricsView(model: model)
                    .frame(maxHeight: .infinity)
                    .transition(.opacity)
            } else {
                LibraryTabs(model: model)
            }
        }
        .padding(12)
        .frame(width: 360, height: 560, alignment: .top)
        .overlay(alignment: .bottom) { ToastBanner(store: model.store) }
        .overlay { ConfirmOverlay(model: model) }
        .animation(.snappy(duration: 0.25), value: model.store.connection)
        .animation(.snappy(duration: 0.25), value: model.store.toasts.first?.id)
        .animation(.snappy(duration: 0.2), value: model.confirm?.id)
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onAppear { focused = true }
        .onKeyPress(.space) {
            guard model.confirm == nil else { return .ignored }
            Task { await model.toggle() }
            return .handled
        }
        .onKeyPress(.escape) {
            if model.confirm != nil {
                model.confirm = nil
            } else {
                closePopover()
            }
            return .handled
        }
    }
}

private struct ClosePopoverKey: EnvironmentKey {
    static var defaultValue: @MainActor () -> Void { {} }
}

extension EnvironmentValues {
    /// Closes the popover that hosts the view (set by `StatusItemController`).
    var closePopover: @MainActor () -> Void {
        get { self[ClosePopoverKey.self] }
        set { self[ClosePopoverKey.self] = newValue }
    }
}
