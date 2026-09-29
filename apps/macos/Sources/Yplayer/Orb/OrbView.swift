import SwiftUI
import YplayerKit

/// The orb in its canvas (`OrbState.canvasSize`, bloom at the right edge): a glass core while a
/// URL drag is on screen, album bubbles around it while the drag is over it, a shake beside a
/// message for a rejected drop, and the "Added to …" toast. Springs animate only while visible.
struct OrbView: View {
    let state: OrbState
    /// Undoes the toast's add.
    var undo: @MainActor () -> Void = {}

    var body: some View {
        let canvas = state.canvasSize
        ZStack(alignment: .topLeading) {
            switch state.phase {
            case .hidden:
                EmptyView()
            case .armed, .bloom:
                bloom
                    .transition(.scale(scale: 0.6).combined(with: .opacity))
            case .shake(let message):
                ShakeRow(message: message, core: state.layout.core)
                    .frame(width: canvas.width, height: canvas.height, alignment: .trailing)
                    .transition(.opacity)
            case .toast(let toast):
                toastCapsule(toast)
                    .frame(width: canvas.width, height: canvas.height, alignment: .trailing)
                    .transition(.scale(scale: 0.8, anchor: .trailing).combined(with: .opacity))
            }
        }
        .frame(width: canvas.width, height: canvas.height, alignment: .topLeading)
        .animation(
            state.phase == .hidden ? nil : .spring(duration: 0.35, bounce: 0.3), value: state.phase)
    }

    /// The core with the bubbles, which fly out of it on bloom and hide behind it while armed.
    private var bloom: some View {
        let hovered: Int?
        let bloomed: Bool
        if case .bloom(let index) = state.phase {
            hovered = index
            bloomed = true
        } else {
            hovered = nil
            bloomed = false
        }
        let layout = state.layout
        let origin = state.layoutOrigin
        let core = CGPoint(x: origin.x + layout.coreCenter.x, y: origin.y + layout.coreCenter.y)
        return ZStack(alignment: .topLeading) {
            ForEach(Array(state.targets.bubbles.enumerated()), id: \.offset) { index, target in
                let center = layout.bubbleCenters[index]
                let isHovered = bloomed && hovered == index
                Bubble(target: target, hovered: isHovered, size: layout.bubble)
                    .scaleEffect(bloomed ? (isHovered ? 1.12 : 1) : 0.3)
                    .opacity(bloomed ? 1 : 0)
                    .position(
                        bloomed ? CGPoint(x: origin.x + center.x, y: origin.y + center.y) : core)
            }
            Core(
                title: bloomed ? state.name(of: state.targets.center) : nil,
                symbol: state.targets.center == .inbox && bloomed ? "tray.fill" : "music.note.list",
                size: layout.core
            )
            .position(core)
        }
    }

    private func toastCapsule(_ toast: AddedToast) -> some View {
        HStack(spacing: 8) {
            if toast.removed {
                Image(systemName: "arrow.uturn.backward.circle.fill")
                    .foregroundStyle(.secondary)
                Text("Removed")
            } else {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                Text("Added to \(toast.album)")
                    .lineLimit(1)
                Text("·")
                    .foregroundStyle(.secondary)
                Button("Undo", action: undo)
                    .buttonStyle(.plain)
                    .fontWeight(.semibold)
                    .foregroundStyle(.tint)
            }
        }
        .font(.callout.weight(.medium))
        .padding(.horizontal, 16)
        .frame(height: 40)
        .glassEffect(.regular, in: .capsule)
    }
}

/// The orb's center: the music symbol, and while bloomed the center target's name below it.
private struct Core: View {
    let title: String?
    let symbol: String
    let size: CGFloat

    var body: some View {
        VStack(spacing: 2) {
            Image(systemName: symbol)
                .font(.system(size: title == nil ? 26 : 16, weight: .semibold))
            if let title {
                Text(title)
                    .font(.caption.bold())
                    .lineLimit(2)
                    .multilineTextAlignment(.center)
                    .frame(width: size - 14)
            }
        }
        .frame(width: size, height: size)
        .glassEffect(.regular, in: .circle)
    }
}

/// One album bubble (or "+ New…"); the hovered one is tinted with the accent color.
private struct Bubble: View {
    let target: OrbTarget
    let hovered: Bool
    let size: CGFloat

    var body: some View {
        Group {
            switch target {
            case .album(_, let name):
                Text(name)
                    .lineLimit(2)
                    .multilineTextAlignment(.center)
            case .inbox:
                Text("Inbox")
            case .newAlbum:
                VStack(spacing: 0) {
                    Image(systemName: "plus")
                        .font(.system(size: 14, weight: .bold))
                    Text("New…")
                }
            }
        }
        .font(.caption.bold())
        .foregroundStyle(hovered ? AnyShapeStyle(.white) : AnyShapeStyle(.primary))
        .frame(width: size - 10)
        .frame(width: size, height: size)
        .background {
            if hovered {
                Circle().fill(.tint)
            }
        }
        .glassEffect(hovered ? .regular.tint(.accentColor) : .regular, in: .circle)
    }
}

/// A rejected drop: the message beside the core, shaking horizontally once for 0.4 s.
private struct ShakeRow: View {
    let message: String
    let core: CGFloat
    @ViewState private var progress: CGFloat = 0

    var body: some View {
        HStack(spacing: 10) {
            Text(message)
                .font(.callout.weight(.semibold))
                .lineLimit(1)
                .padding(.horizontal, 16)
                .frame(height: 40)
                .glassEffect(.regular, in: .capsule)
            Image(systemName: "xmark")
                .font(.system(size: 24, weight: .semibold))
                .frame(width: core, height: core)
                .glassEffect(.regular.tint(.red.opacity(0.35)), in: .circle)
        }
        .modifier(Shake(progress: progress))
        .onAppear {
            withAnimation(.linear(duration: 0.4)) { progress = 1 }
        }
    }
}

/// Three horizontal swings over `progress` 0 → 1.
private struct Shake: GeometryEffect {
    var progress: CGFloat

    var animatableData: CGFloat {
        get { progress }
        set { progress = newValue }
    }

    func effectValue(size: CGSize) -> ProjectionTransform {
        ProjectionTransform(
            CGAffineTransform(translationX: 10 * sin(progress * .pi * 6), y: 0))
    }
}
