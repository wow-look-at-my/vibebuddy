import KorekoreUI
import SwiftUI
import VibeBuddyCore

/// The General page (canvas: "Mac · General").
struct KorekoreGeneralPage: View {
    @ObservedObject var model: AppModel
    @State private var language = AppLanguage.current

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                KorekoreOwnKeysCard(model: model)
                Text("This Mac").font(.rounded(15)).padding(.leading, 4)
                KRows {
                    KRow(String(localized: "Language")) {
                        // The dialog runs after this update rather than as a nested modal loop inside the menu's action.
                        KMenu(selection: language, options: [(.system, String(localized: "System")), (.en, "English"), (.zhHans, "简体中文")]) { choice in
                            DispatchQueue.main.async { change(to: choice) }
                        }
                    }
                    KDivider()
                    KRow(String(localized: "Launch at login")) {
                        KToggle(String(localized: "Launch at login"), isOn: Binding(get: { model.launchAtLogin }, set: { model.setLaunchAtLogin($0) }))
                    }
                    KDivider()
                    KRow(String(localized: "Tell me when the box disconnects or the daemon fails")) {
                        KToggle(String(localized: "Tell me when the box disconnects or the daemon fails"),
                                isOn: Binding(get: { model.status?.config.notifyLink ?? true }, set: { model.setNotifyLink($0) }))
                            .disabled(model.status == nil)
                    }
                }
                KCard(padding: EdgeInsets(top: 14, leading: 16, bottom: 14, trailing: 16)) {
                    HStack(spacing: 14) {
                        KoreBadge(fill: Theme.Colors.korePink)
                            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(Theme.Colors.ink, lineWidth: Theme.Stroke.base))
                        VStack(alignment: .leading, spacing: 3) {
                            HStack(alignment: .bottom, spacing: 8) {
                                Text(verbatim: "korekore").font(.rounded(18))
                                PixelTag()
                            }
                            KTag("App \(Resources.displayVersion)", color: Theme.Colors.muted)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        Button(String(localized: "Report a problem…")) { model.reportProblem() }.kButton()
                    }
                }
            }
            .padding(.bottom, Theme.Sticker.sm + 4)
        }
        .scrollIndicators(.never)
    }

    /// Saves the choice, then offers to restart and, when the box speaks the other language, to switch its voice too.
    private func change(to choice: AppLanguage) {
        guard choice != language else { return }
        choice.save()
        language = choice
        let target = choice.resolved
        guard target != Resources.uiLanguage else { return }
        var voiceSwitch: VoiceCatalogEntry?
        var checkbox: KorekoreAlert.Checkbox?
        var note = String(localized: "The box goes offline for a few seconds while the app restarts.")
        let device = model.status?.device
        // Only a box that has answered (build and voice reported) can take a voice; an unknown voice isn't guessed at.
        if let device, device.connected, device.firmwareBuild != nil, let voice = device.voice,
           let entry = VoiceCatalogEntry.switchSuggestion(boxVoice: voice, to: target, bundled: Resources.bundledVoices) {
            let label = device.bridge
                ? String(localized: "Also switch the box's voice to \(entry.name) (a few minutes over the UART bridge; keep it plugged in)")
                : String(localized: "Also switch the box's voice to \(entry.name)")
            checkbox = KorekoreAlert.Checkbox(label: label, isOn: true)
            voiceSwitch = entry
        } else if !(device?.connected ?? false) {
            note += "\n" + String(localized: "The box isn't connected, so its character stays as it is. You can change it later on the Character tab.")
        }
        KorekoreAlert.ask(title: String(localized: "Restart korekore to change the language?"), message: note,
                          buttons: [String(localized: "Restart"), String(localized: "Not now")], checkbox: checkbox) { [model, voiceSwitch] answer in
            let voice = answer.checked ? voiceSwitch?.id : nil
            if answer.button == 0 {
                model.restart(writingVoice: voice)
            } else if let voice {
                model.writeVoice(voice)
            }
        }
    }
}
