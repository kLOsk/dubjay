//
//  PreferencesSheet.swift
//  Dub
//
//  M10.3 Preferences sheet. Houses the old M10-B / M10.2 dev
//  toolbar — input device picker, deck A/B channel fields, palette
//  picker, Start/Stop — *off* the performance surface so the
//  performance view stays clean and so a stage-mode DJ can never
//  hit a "Start/Stop" button by accident.
//
//  Opened via `⌘,` (the macOS standard "open Preferences" shortcut)
//  from `MainView`. Until M18 the sheet is the *only* way to start
//  or stop the engine; the performance surface is read-only.
//
//  We deliberately keep this sheet a one-pane affair rather than a
//  tabbed `Settings` scene — that pattern arrives with M18 polish
//  when there's more than one config domain to organise.
//

import SwiftUI
import DubCore

struct PreferencesSheet: View {

    @ObservedObject var model: WaveformAppModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: DubSpacing.lg) {
            header
            Divider()
            deviceSection
            channelsSection
            paletteSection
            Spacer(minLength: 0)
            Divider()
            footer
        }
        .padding(DubSpacing.xl)
        .frame(width: 520, height: 480)
        .background(DubColor.surface0)
    }

    // MARK: - Sections

    private var header: some View {
        HStack {
            Text("Preferences")
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(DubColor.textPrimary)
            Spacer()
            Text("M10.3 dev surface — final preferences UX in M18")
                .font(DubFont.micro)
                .foregroundStyle(DubColor.textPlaceholder)
        }
    }

    private var deviceSection: some View {
        section(title: "INPUT DEVICE") {
            HStack(spacing: DubSpacing.sm) {
                Picker("", selection: pickerBinding) {
                    if model.availableDevices.isEmpty {
                        Text("No input devices found").tag(Optional<String>.none)
                    } else {
                        ForEach(model.availableDevices, id: \.self) { d in
                            Text(d).tag(Optional<String>.some(d))
                        }
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .disabled(model.isRunning)

                Button {
                    model.refreshDevices()
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .help("Re-scan input devices")
                .disabled(model.isRunning)
            }
        }
    }

    private var channelsSection: some View {
        section(title: "CHANNEL PAIRS (1-BASED)") {
            VStack(alignment: .leading, spacing: DubSpacing.sm) {
                channelField(
                    label: "Deck A",
                    text: $model.channelsAText,
                    hint: "1,2")
                channelField(
                    label: "Deck B",
                    text: $model.channelsBText,
                    hint: "leave empty for single-deck")
                Text("E.g. SL3: A = 3,4 · B = 5,6.")
                    .font(DubFont.micro)
                    .foregroundStyle(DubColor.textTertiary)
            }
        }
    }

    private var paletteSection: some View {
        section(title: "WAVEFORM PALETTE") {
            Picker("", selection: $model.palette) {
                ForEach(WaveformPalette.allCases) { p in
                    Text(p.displayName).tag(p)
                }
            }
            .labelsHidden()
            .pickerStyle(.segmented)
        }
    }

    private var footer: some View {
        HStack(spacing: DubSpacing.md) {
            if let err = model.lastError {
                Text(err)
                    .font(DubFont.micro)
                    .foregroundStyle(DubColor.stateError)
                    .lineLimit(2)
            }
            Spacer(minLength: 0)
            if model.isRunning {
                Button("Stop", role: .destructive) {
                    model.stop()
                }
                .keyboardShortcut(.escape, modifiers: [])
            } else {
                Button("Start") {
                    model.start()
                    if model.lastError == nil {
                        dismiss()
                    }
                }
                .keyboardShortcut(.return, modifiers: [])
                .disabled(model.selectedDevice == nil)
            }
            Button("Close") { dismiss() }
                .keyboardShortcut(.cancelAction)
        }
    }

    // MARK: - Helpers

    @ViewBuilder
    private func section<Content: View>(
        title: String,
        @ViewBuilder _ content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: DubSpacing.sm) {
            Text(title)
                .font(DubFont.caps)
                .tracking(1.0)
                .foregroundStyle(DubColor.textSecondary)
            content()
        }
    }

    @ViewBuilder
    private func channelField(
        label: String, text: Binding<String>, hint: String
    ) -> some View {
        HStack(spacing: DubSpacing.sm) {
            Text(label)
                .font(DubFont.body)
                .foregroundStyle(DubColor.textPrimary)
                .frame(width: 64, alignment: .leading)
            TextField(hint, text: text)
                .textFieldStyle(.roundedBorder)
                .frame(width: 240)
                .disabled(model.isRunning)
        }
    }

    private var pickerBinding: Binding<String?> {
        Binding(
            get: { model.selectedDevice },
            set: { model.selectedDevice = $0 }
        )
    }
}

#Preview {
    PreferencesSheet(model: WaveformAppModel())
}
