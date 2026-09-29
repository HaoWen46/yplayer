import SwiftUI
import YplayerKit

/// Shown while not connected: the retry countdown with Retry Now, or "Connecting…".
struct ConnectionBanner: View {
    let model: AppModel

    var body: some View {
        HStack(spacing: 10) {
            switch model.store.connection {
            case .disconnected(let retryAt):
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                TimelineView(.periodic(from: .now, by: 1)) { context in
                    let seconds = max(0, Int(retryAt.timeIntervalSince(context.date).rounded(.up)))
                    Text("Can't reach the yplay service — retrying in \(seconds)s")
                        .font(.callout)
                        .lineLimit(2)
                }
                Spacer(minLength: 0)
                Button("Retry Now") {
                    Task { await model.client.start() }
                }
                .buttonStyle(.glass)
                .controlSize(.small)
            case .connecting:
                ProgressView()
                    .controlSize(.small)
                Text("Connecting to the yplay service…")
                    .font(.callout)
                Spacer(minLength: 0)
            case .connected:
                EmptyView()
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .glassEffect(.regular.tint(.orange.opacity(0.15)), in: .rect(cornerRadius: 14))
    }
}
