import SwiftUI
import YplayerKit

/// Previous · play/pause · next as glass circles; play/pause is the larger one.
struct TransportControls: View {
    let model: AppModel
    let isPlaying: Bool

    var body: some View {
        HStack(spacing: 14) {
            button("Previous", "backward.fill", font: .title3, size: 30) {
                await model.prev()
            }
            button(
                isPlaying ? "Pause" : "Play", isPlaying ? "pause.fill" : "play.fill",
                font: .title, size: 42
            ) {
                await model.toggle()
            }
            button("Next", "forward.fill", font: .title3, size: 30) {
                await model.next()
            }
        }
    }

    private func button(
        _ title: String, _ symbol: String, font: Font, size: CGFloat,
        action: @escaping @MainActor () async -> Void
    ) -> some View {
        Button {
            Task { await action() }
        } label: {
            Label(title, systemImage: symbol)
                .labelStyle(.iconOnly)
                .font(font)
                .frame(width: size, height: size)
                .contentTransition(.symbolEffect(.replace))
        }
        .buttonStyle(.glass)
        .buttonBorderShape(.circle)
    }
}
