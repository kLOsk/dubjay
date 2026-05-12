//
//  MainView.swift
//  Dub
//
//  M10.3 top-level SwiftUI view. The performance surface is now
//  `PerformanceView`; this file shrinks to:
//
//  * `WaveformAppModel` — the engine handle + UI state (unchanged
//    in semantics from M10.2; just moved here from its old toolbar
//    role).
//  * `MainView`         — composes `PerformanceView` + the
//                          `⌘,` Preferences sheet.
//
//  See `PerformanceView.swift` for the actual UI layout per
//  PRD §9.2 and `PreferencesSheet.swift` for the dev controls.
//

import SwiftUI
import DubCore

/// View-model owning the shared `DubEngine` for the lifetime of the
/// app window. All mutations happen on the main actor (`@MainActor`).
///
/// Behaviour is identical to M10.2's `WaveformAppModel`; the only
/// change for M10.3 is that *consumers* moved (the dev toolbar is
/// gone, the Preferences sheet binds to the same fields, the
/// Performance View reads `isRunning` / `twoDeckMode` / `palette`).
@MainActor
final class WaveformAppModel: ObservableObject {

    let engine: DubEngine

    @Published var availableDevices: [String] = []
    @Published var selectedDevice: String? = nil
    @Published var channelsAText: String = "1,2"
    /// Empty = single-deck mode (M10-B default). Filling it in
    /// switches `start()` to `startThruTwoDeck`.
    @Published var channelsBText: String = ""
    @Published var isRunning: Bool = false
    @Published var lastError: String? = nil
    @Published var palette: WaveformPalette = .seratoFaithful
    /// True when the most recent `start()` opened the engine in
    /// 2-deck mode. Drives whether `PerformanceView` shows the
    /// deck-B waveform pane.
    @Published private(set) var twoDeckMode: Bool = false

    init() {
        self.engine = DubEngine()
        refreshDevices()
    }

    deinit {
        engine.stopThru()
    }

    func refreshDevices() {
        availableDevices = engine.listInputDevices()
        if selectedDevice == nil, let first = availableDevices.first {
            selectedDevice = first
        }
    }

    func start() {
        lastError = nil
        guard let device = selectedDevice, !device.isEmpty else {
            lastError = "Pick an input device first."
            return
        }
        let channelsA: [UInt32]
        switch parseChannels(channelsAText, side: "A") {
        case .success(let cs): channelsA = cs
        case .failure(let msg):
            lastError = msg
            return
        }

        let trimmedB = channelsBText.trimmingCharacters(in: .whitespaces)
        do {
            if trimmedB.isEmpty {
                try engine.startThru(deviceName: device, channels: channelsA)
                twoDeckMode = false
            } else {
                let channelsB: [UInt32]
                switch parseChannels(trimmedB, side: "B") {
                case .success(let cs): channelsB = cs
                case .failure(let msg):
                    lastError = msg
                    return
                }
                try engine.startThruTwoDeck(
                    deviceName: device,
                    channelsA: channelsA,
                    channelsB: channelsB)
                twoDeckMode = true
            }
            isRunning = true
        } catch let error as EngineError {
            lastError = describe(error)
        } catch {
            lastError = "Unexpected error: \(error.localizedDescription)"
        }
    }

    func stop() {
        engine.stopThru()
        isRunning = false
        twoDeckMode = false
    }

    // MARK: - Helpers

    private enum ParseResult {
        case success([UInt32])
        case failure(String)
    }

    private func parseChannels(_ text: String, side: String) -> ParseResult {
        let parts = text.split(separator: ",").map {
            $0.trimmingCharacters(in: .whitespaces)
        }
        guard parts.count == 2 else {
            return .failure(
                "Deck \(side) channels need exactly two values, e.g. '1,2' or '3,4'.")
        }
        var out: [UInt32] = []
        for p in parts {
            guard let v = UInt32(p), v >= 1 else {
                return .failure(
                    "Deck \(side): '\(p)' is not a 1-based channel number.")
            }
            out.append(v)
        }
        return .success(out)
    }

    private func describe(_ error: EngineError) -> String {
        switch error {
        case .DeviceNotFound:    return "Device not found."
        case .InvalidChannels:   return "Invalid / overlapping channels — use two 1-based numbers per deck."
        case .AudioStartFailed:  return "Audio start failed."
        case .AlreadyRunning:    return "Engine already running."
        case .NotRunning:        return "Engine not running."
        case .InvalidDeckIndex:  return "Invalid deck index."
        }
    }
}

/// Top-level shell: the performance surface plus a `⌘,`-triggered
/// Preferences sheet. The Preferences sheet is the *only* way to
/// start / stop the engine in M10.3 — the performance surface is
/// read-only, by design (PRD §2 "No Mouse DJ Ever").
struct MainView: View {

    @StateObject private var model = WaveformAppModel()
    @State private var showingPreferences: Bool = false

    var body: some View {
        PerformanceView(model: model)
            .frame(minWidth: 960, minHeight: 600)
            .sheet(isPresented: $showingPreferences) {
                PreferencesSheet(model: model)
            }
            .background(
                CommandShortcuts(showingPreferences: $showingPreferences)
            )
            .onAppear {
                // Auto-open Preferences on first launch when the
                // engine hasn't been configured yet. Avoids the
                // "blank screen, what do I do?" trap. Once the
                // user has started a session, subsequent app
                // launches still open into the performance view —
                // but for M10.3 we don't persist the last config,
                // so cold launch always lands in idle.
                if !model.isRunning {
                    showingPreferences = true
                }
            }
    }
}

// MARK: - Keyboard shortcuts

/// Invisible host that registers app-level keyboard shortcuts. We
/// can't put `.keyboardShortcut` on a `.sheet` modifier directly,
/// so we attach it to an empty `Button` that toggles the binding.
private struct CommandShortcuts: View {
    @Binding var showingPreferences: Bool

    var body: some View {
        Button("Preferences") {
            showingPreferences.toggle()
        }
        .keyboardShortcut(",", modifiers: .command)
        .frame(width: 0, height: 0)
        .opacity(0)
        .accessibilityHidden(true)
    }
}

#Preview {
    MainView()
        .frame(width: 1440, height: 900)
}
