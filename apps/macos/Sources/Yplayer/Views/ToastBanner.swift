import SwiftUI
import YplayerKit

/// The oldest toast younger than `LibraryStore.toastLifetime` as a bottom banner; dismissed
/// after 4 s (errors 6 s) by a one-shot sleep that exists only while the banner is visible.
struct ToastBanner: View {
    let store: LibraryStore

    var body: some View {
        if let toast = store.toasts.first(where: {
            Date.now.timeIntervalSince($0.createdAt) < LibraryStore.toastLifetime
        }) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: Self.symbol(toast.severity))
                    .foregroundStyle(Self.tint(toast.severity))
                Text(toast.message)
                    .font(.callout)
                    .lineLimit(3)
                Spacer(minLength: 0)
                Button {
                    dismiss(toast.id)
                } label: {
                    Image(systemName: "xmark")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .glassEffect(.regular, in: .rect(cornerRadius: 14))
            .padding(12)
            .transition(.move(edge: .bottom).combined(with: .opacity))
            .task(id: toast.id) {
                do {
                    try await Task.sleep(for: .seconds(toast.severity == .error ? 6 : 4))
                } catch {
                    return
                }
                dismiss(toast.id)
            }
        }
    }

    private func dismiss(_ id: UUID) {
        store.toasts.removeAll { $0.id == id }
    }

    private static func symbol(_ severity: Severity) -> String {
        switch severity {
        case .info: "info.circle.fill"
        case .warn: "exclamationmark.triangle.fill"
        case .error: "xmark.octagon.fill"
        }
    }

    private static func tint(_ severity: Severity) -> Color {
        switch severity {
        case .info: .accentColor
        case .warn: .orange
        case .error: .red
        }
    }
}
