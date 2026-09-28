import SwiftUI
import YplayerKit

/// `model.confirm` as a dimmed backdrop and a centered card (in place of an NSAlert, so the
/// transient popover keeps key status). Esc cancels; there is no default button.
struct ConfirmOverlay: View {
    let model: AppModel

    var body: some View {
        if let request = model.confirm {
            ZStack {
                Rectangle()
                    .fill(.black.opacity(0.3))
                    .ignoresSafeArea()
                card(request)
            }
            .transition(.opacity)
        }
    }

    private func card(_ request: ConfirmRequest) -> some View {
        VStack(spacing: 16) {
            VStack(spacing: 6) {
                Text(request.title)
                    .font(.headline)
                Text(request.message)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            .multilineTextAlignment(.center)
            .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 10) {
                Button(role: .cancel) {
                    model.confirm = nil
                } label: {
                    Text("Cancel").frame(maxWidth: .infinity)
                }
                .buttonStyle(.glass)
                .keyboardShortcut(.cancelAction)
                Button(role: .destructive) {
                    model.confirm = nil
                    Task { await request.action() }
                } label: {
                    Text(request.actionTitle).frame(maxWidth: .infinity)
                }
                .buttonStyle(.glass)
                .foregroundStyle(.red)
            }
            .controlSize(.large)
        }
        .padding(20)
        .frame(width: 296)
        .glassEffect(.regular, in: .rect(cornerRadius: 22))
    }
}
