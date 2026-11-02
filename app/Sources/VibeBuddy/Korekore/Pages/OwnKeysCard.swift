import KorekoreUI
import SwiftUI
import VibeBuddyCore

/// The user's own keys (ADR-0020), at the top of the General page: spoken replies and making a Character need one.
struct KorekoreOwnKeysCard: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Your own keys").font(.rounded(15)).padding(.leading, 4)
            KRows {
                if let keys = model.status?.voiceKeys {
                    row {
                        KNote(String(localized: "Spoken replies and making a Character need your own ElevenLabs key, billed to your account there."))
                    }
                    KDivider()
                    KorekoreOwnKeyRow(title: "ElevenLabs", key: keys.elevenlabs, keyPage: Resources.elevenLabsKeys,
                                      refused: String(localized: "ElevenLabs refused your own key. Replies in its voices stay silent until you set another one.")) { key in
                        await saveKey(key)
                    }
                } else {
                    row { KNote(String(localized: "The daemon isn't running. Restart it on the Advanced tab.")) }
                }
            }
        }
    }

    private func row<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        HStack(spacing: 12) { content() }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.vertical, 12)
            .padding(.horizontal, 16)
    }

    /// The status stream then shows the key as set or not; true when the daemon took it.
    private func saveKey(_ key: String?) async -> Bool {
        do {
            try await model.client.setVoiceKey(key)
            return true
        } catch {
            model.lastError = String(localized: "Couldn't save your own key: \(error.localizedDescription)")
            return false
        }
    }
}

/// The user's own key (ADR-0020). It is typed once and never shown again: the daemon only says
/// whether it has one.
struct KorekoreOwnKeyRow: View {
    let title: String
    let key: VoiceKeysStatus.Key
    let keyPage: URL
    let refused: String
    /// Sends the key, or nil to clear it; true when the daemon took it.
    let save: (String?) async -> Bool
    @State private var draft = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Text(verbatim: title).font(.rounded(14))
                Spacer()
                if key.set {
                    KTag(String(localized: "Set"), color: Theme.Colors.doneText)
                    Button(String(localized: "Clear")) { Task { _ = await save(nil) } }.kButton()
                } else {
                    KField(String(localized: "Your own key"), text: $draft, secure: true)
                        .frame(maxWidth: 240)
                        .onSubmit { submit() }
                    Button(String(localized: "Save")) { submit() }.kButton(.sun)
                        .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                Link(destination: keyPage) { Text("Get a key").font(.rounded(12)).underline() }
                    .foregroundStyle(Theme.Colors.korePinkText)
            }
            if key.refused {
                KTag(refused, color: Theme.Colors.korePinkText)
            }
        }
        .padding(.vertical, 11)
        .padding(.horizontal, 16)
    }

    private func submit() {
        let typed = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !typed.isEmpty else { return }
        Task { if await save(typed) { draft = "" } }
    }
}
