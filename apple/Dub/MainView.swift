//
//  MainView.swift
//  Dub
//
//  Top-level SwiftUI view + the app-wide `WaveformAppModel` view
//  model that owns the shared `DubEngine` handle.
//
//  M10.5b refactor: the model is no longer just an engine
//  start/stop wrapper. It owns per-deck state (track info, position,
//  is-playing, load-error flash), drives a 30 Hz polling timer that
//  reads `engine.position(deck)` to keep the deck headers in sync,
//  derives the master deck per PRD §6.4, exposes the FS-browser
//  selection that `Space` loads, and routes drag-and-drop URLs to
//  the engine via `load_track + play`.
//
//  See `PerformanceView.swift` for the actual layout per PRD §9.2
//  and `PreferencesSheet.swift` for the engine-lifecycle controls.
//

import AppKit
import Combine
import SwiftUI
import UniformTypeIdentifiers

import DubCore

/// Mode the engine is currently running in. Drives whether the
/// canonical two-deck performance surface (`.timecode`) or the
/// single-deck track-prep shell (`.prep`) is shown, and which
/// `DubEngine` lifecycle entry point gets called on Start.
///
/// PRD §3.1: auto-detect picks the default at launch; user can
/// override in Preferences.
enum EngineMode: String, CaseIterable, Identifiable {
    case timecode = "timecode"
    case prep = "prep"

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .timecode: return "Performance (Timecode)"
        case .prep:     return "Track Preparation"
        }
    }
}

/// Per-deck UI state. Driven by the model's 30 Hz polling loop +
/// load-track / play / pause calls. The performance view is a
/// pure function of one of these per deck.
///
/// All time values are wall-clock seconds. `nil`-able fields are
/// `nil` when the deck has no track loaded; the deck header
/// renders em-dashes in that case.
struct DeckState: Equatable {
    /// True once `load_track` has succeeded on this deck. Cleared
    /// when the engine stops or a load fails.
    var hasTrack: Bool = false

    /// True when the engine is advancing the playhead. Driven by
    /// the 30 Hz poll, not the UI's local pause/play state — keeps
    /// the chrome honest with the engine.
    var isPlaying: Bool = false

    /// True after the playhead reaches the end of the track. The
    /// engine stays at end (auto-stop, not auto-rewind) per PRD
    /// §6.1.3.
    var atEnd: Bool = false

    /// Filename stem of the loaded track. Title / artist tags are
    /// M11 work — for M10.5b we show the file basename so the DJ
    /// can tell which track they loaded.
    var displayName: String? = nil

    /// Format / SR chip ("MP3 · 44.1 kHz · 2 ch"). `nil` until a
    /// track loads.
    var formatChip: String? = nil

    /// Total track duration. 0 if no track loaded.
    var durationSecs: Double = 0

    /// Wall-clock elapsed from track start. 0 if no track.
    var elapsedSecs: Double = 0

    /// Wall-clock remaining to track end. 0 if no track.
    var remainingSecs: Double = 0

    /// When set in the future, the deck pane renders a red overlay
    /// with a "deck is playing — lift the needle" message until
    /// this timestamp elapses. Used to surface a load failure
    /// caused by attempting to load into a playing deck (PRD §5.5
    /// + §6.4).
    var errorFlashUntil: Date? = nil

    /// Cached source URL for the loaded file. Used by drag-drop
    /// targeting + the FS browser to highlight which file is
    /// loaded on each deck.
    var sourceURL: URL? = nil

    static let empty = DeckState()

    /// `true` when the deck has a track but isn't currently
    /// playing — a valid target for `Space` load (PRD §6.4 + §5.5).
    var isStopped: Bool { !isPlaying }
}

/// View-model owning the shared `DubEngine` for the lifetime of the
/// app window. All mutations happen on the main actor (`@MainActor`).
@MainActor
final class WaveformAppModel: ObservableObject {

    // MARK: Engine handle

    let engine: DubEngine

    // MARK: Lifecycle config (driven by Preferences)

    @Published var availableDevices: [String] = []
    @Published var selectedDevice: String? = nil
    @Published var channelsAText: String = "1,2"
    /// Empty = single-deck mode (only in `.timecode`); always
    /// ignored in `.prep` (deck B stays off).
    @Published var channelsBText: String = ""
    @Published var palette: WaveformPalette = .seratoFaithful

    /// Engine mode the next Start call will use. Auto-default
    /// computed at launch; user can override in Preferences.
    @Published var engineMode: EngineMode = .timecode

    // MARK: Live engine state

    @Published private(set) var isRunning: Bool = false
    @Published var lastError: String? = nil
    /// True iff the most recent Start opened the engine in
    /// two-deck mode (Timecode + non-empty deck-B channels).
    @Published private(set) var twoDeckMode: Bool = false

    // MARK: Per-deck state (M10.5b)

    @Published private(set) var deckA: DeckState = .empty
    @Published private(set) var deckB: DeckState = .empty

    /// Master deck per PRD §6.4 (sticky single-master). `nil` only
    /// while the engine is stopped.
    @Published private(set) var masterDeck: DeckSide? = nil

    // MARK: FS-browser selection (M10.5b)

    /// File the user has highlighted in the FS browser. `Space`
    /// loads this into the non-master, stopped deck (PRD §5.5).
    @Published var browserSelection: URL? = nil

    // MARK: Private state

    /// Sticky master from the previous round when neither deck is
    /// currently playing. Starts at `.a` so the cold-launch UI has
    /// a definite anchor.
    private var stickyMaster: DeckSide = .a
    private var lastPlayStart: [DeckSide: Date] = [:]

    /// Polling timer for `engine.position(deck)`. ~30 Hz keeps the
    /// track-time row smooth without hammering the FFI; the
    /// audio-thread playhead is sampled by the timer-published
    /// snapshot inside `RunningState`. Disabled when the engine
    /// isn't running.
    private var pollTimer: Timer?
    private static let pollIntervalSecs: TimeInterval = 1.0 / 30.0

    // MARK: Init / deinit

    init() {
        self.engine = DubEngine()
        refreshDevices()
        applyAutoDetect()
    }

    deinit {
        engine.stopEngine()
    }

    // MARK: Device list + auto-detect

    func refreshDevices() {
        availableDevices = engine.listInputDevices()
        if selectedDevice == nil, let first = availableDevices.first {
            selectedDevice = first
        }
    }

    /// Pick a default `engineMode` based on what's plugged in. M10.5b
    /// heuristic: if the system reports *any* input device beyond the
    /// built-in mic (commonly named "MacBook Pro Microphone",
    /// "External Microphone", or "Built-in Microphone"), assume
    /// Timecode mode; otherwise default to Prep mode (file-only).
    ///
    /// This is intentionally crude — properly enumerating input
    /// channel counts requires an FFI extension we defer to M10.5b's
    /// follow-up. The user can override in Preferences either way.
    private func applyAutoDetect() {
        let nonBuiltin = availableDevices.filter { !isLikelyBuiltinMic($0) }
        engineMode = nonBuiltin.isEmpty ? .prep : .timecode
    }

    private func isLikelyBuiltinMic(_ name: String) -> Bool {
        let lower = name.lowercased()
        return lower.contains("built-in") || lower.contains("internal")
            || lower.contains("macbook") || lower.contains("imac")
            || lower.contains("mac mini") || lower.contains("mac studio")
    }

    // MARK: Engine lifecycle

    func start() {
        lastError = nil
        switch engineMode {
        case .timecode: startTimecode()
        case .prep:     startPrep()
        }
        if isRunning { startPolling() }
    }

    func stop() {
        stopPolling()
        engine.stopEngine()
        isRunning = false
        twoDeckMode = false
        deckA = .empty
        deckB = .empty
        masterDeck = nil
        stickyMaster = .a
        lastPlayStart.removeAll()
    }

    private func startTimecode() {
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
                    deviceName: device, channelsA: channelsA, channelsB: channelsB)
                twoDeckMode = true
            }
            isRunning = true
            masterDeck = stickyMaster
        } catch let error as EngineError {
            lastError = describe(error)
        } catch {
            lastError = "Unexpected error: \(error.localizedDescription)"
        }
    }

    private func startPrep() {
        do {
            try engine.startEngine(outputChannels: 2)
            isRunning = true
            twoDeckMode = false
            masterDeck = stickyMaster
        } catch let error as EngineError {
            lastError = describe(error)
        } catch {
            lastError = "Unexpected error: \(error.localizedDescription)"
        }
    }

    // MARK: Polling

    private func startPolling() {
        stopPolling()
        // Use a tolerance so the timer can coalesce with other
        // main-runloop work; 30 Hz is the *target*, slightly less
        // is fine for the track-time row.
        let timer = Timer.scheduledTimer(
            withTimeInterval: Self.pollIntervalSecs, repeats: true
        ) { [weak self] _ in
            Task { @MainActor [weak self] in self?.pollDecks() }
        }
        timer.tolerance = Self.pollIntervalSecs * 0.25
        RunLoop.main.add(timer, forMode: .common)
        pollTimer = timer
    }

    private func stopPolling() {
        pollTimer?.invalidate()
        pollTimer = nil
    }

    private func pollDecks() {
        guard isRunning else { return }
        let newA = readDeckState(side: .a, prev: deckA)
        let newB = readDeckState(side: .b, prev: deckB)
        if newA != deckA { deckA = newA }
        if newB != deckB { deckB = newB }
        recomputeMaster()
    }

    private func readDeckState(side: DeckSide, prev: DeckState) -> DeckState {
        let pos = engine.position(deckIdx: side.ffiDeckIdx)
        let nowPlaying = pos.isPlaying
        if nowPlaying, !prev.isPlaying {
            lastPlayStart[side] = Date()
        }
        var next = prev
        next.hasTrack = pos.hasTrack
        next.isPlaying = nowPlaying
        next.atEnd = pos.atEnd
        next.durationSecs = pos.durationSecs
        next.elapsedSecs = pos.elapsedSecs
        next.remainingSecs = pos.remainingSecs
        // Clear stale error flash once it elapses; the deck pane
        // will hide the overlay automatically when it observes
        // `Date() > errorFlashUntil`.
        if let until = next.errorFlashUntil, Date() >= until {
            next.errorFlashUntil = nil
        }
        return next
    }

    // MARK: Master deck (PRD §6.4)

    private func recomputeMaster() {
        let aPlaying = deckA.isPlaying
        let bPlaying = deckB.isPlaying
        let newMaster: DeckSide
        switch (aPlaying, bPlaying) {
        case (true, false): newMaster = .a
        case (false, true): newMaster = .b
        case (true, true):
            let aTs = lastPlayStart[.a] ?? .distantPast
            let bTs = lastPlayStart[.b] ?? .distantPast
            newMaster = (aTs >= bTs) ? .a : .b
        case (false, false): newMaster = stickyMaster
        }
        if masterDeck != newMaster {
            masterDeck = newMaster
        }
        stickyMaster = newMaster
    }

    // MARK: Track load + transport

    /// Load a track onto `side`. Refuses (and red-flashes the deck
    /// pane) if the target deck is currently playing, per PRD §5.5
    /// + §6.4 — the user must lift the needle / pause first.
    ///
    /// Returns `true` on a successful load (the deck is now armed
    /// and ready; caller can follow up with `play(side)` to start
    /// Casual Play, but Space-load is intentionally load-only).
    @discardableResult
    func loadTrack(side: DeckSide, url: URL) -> Bool {
        guard isRunning else {
            lastError = "Engine not running. Start it from Preferences (⌘,)."
            return false
        }
        let target = state(for: side)
        if target.isPlaying {
            flashLoadError(side: side)
            return false
        }
        do {
            try engine.loadTrack(deckIdx: side.ffiDeckIdx, path: url.path)
            var next = target
            next.hasTrack = true
            next.atEnd = false
            next.isPlaying = false
            next.elapsedSecs = 0
            next.remainingSecs = 0
            next.sourceURL = url
            next.displayName = url.deletingPathExtension().lastPathComponent
            if let info = engine.trackInfo(deckIdx: side.ffiDeckIdx) {
                next.durationSecs = info.durationSecs
                next.formatChip = formatChip(for: url, info: info)
            }
            next.errorFlashUntil = nil
            setState(next, for: side)
            recomputeMaster()
            return true
        } catch let error as EngineError {
            lastError = describe(error)
            return false
        } catch {
            lastError = "Unexpected load error: \(error.localizedDescription)"
            return false
        }
    }

    /// Load the FS-browser selection into the non-master, stopped
    /// deck. PRD §5.5 — bound to `Space` in `MainView`.
    func loadBrowserSelectionIntoNonMaster() {
        guard isRunning else { return }
        guard let url = browserSelection else {
            lastError = "Select a file in the browser first."
            return
        }
        let candidate = nonMasterSide()
        let target = state(for: candidate)
        if target.isPlaying {
            flashLoadError(side: candidate)
            return
        }
        _ = loadTrack(side: candidate, url: url)
    }

    /// Returns the deck that is *not* the master — Space-load
    /// targets this one (PRD §6.4). When `masterDeck == nil` (e.g.
    /// just after launch, before Start), fall back to `stickyMaster`
    /// so the load still picks a sane deck.
    private func nonMasterSide() -> DeckSide {
        let m = masterDeck ?? stickyMaster
        return m == .a ? .b : .a
    }

    func play(side: DeckSide) {
        guard isRunning else { return }
        do {
            try engine.play(deckIdx: side.ffiDeckIdx)
            lastPlayStart[side] = Date()
            var s = state(for: side)
            s.isPlaying = true
            setState(s, for: side)
            recomputeMaster()
        } catch let error as EngineError {
            lastError = describe(error)
        } catch {
            lastError = "Play failed: \(error.localizedDescription)"
        }
    }

    func pause(side: DeckSide) {
        guard isRunning else { return }
        do {
            try engine.pause(deckIdx: side.ffiDeckIdx)
            var s = state(for: side)
            s.isPlaying = false
            setState(s, for: side)
            recomputeMaster()
        } catch let error as EngineError {
            lastError = describe(error)
        } catch {
            lastError = "Pause failed: \(error.localizedDescription)"
        }
    }

    // MARK: Helpers

    private func flashLoadError(side: DeckSide) {
        // 200 ms red flash per PRD §5.5: "deck is playing — lift the
        // needle". Long enough to register, short enough not to
        // intrude on the next attempt.
        var s = state(for: side)
        s.errorFlashUntil = Date().addingTimeInterval(0.2)
        setState(s, for: side)
    }

    private func state(for side: DeckSide) -> DeckState {
        switch side {
        case .a: return deckA
        case .b: return deckB
        }
    }

    private func setState(_ next: DeckState, for side: DeckSide) {
        switch side {
        case .a: deckA = next
        case .b: deckB = next
        }
    }

    private func formatChip(for url: URL, info: TrackInfo) -> String {
        let ext = url.pathExtension.uppercased()
        let sr = String(format: "%.1f kHz", Double(info.sampleRate) / 1000.0)
        let ch = info.channels == 1 ? "mono" : "stereo"
        return "\(ext) · \(sr) · \(ch)"
    }

    // MARK: Channel parsing

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
        case .DeviceNotFound:       return "Device not found."
        case .InvalidChannels:      return "Invalid / overlapping channels — use two 1-based numbers per deck."
        case .AudioStartFailed:     return "Audio start failed."
        case .AlreadyRunning:       return "Engine already running."
        case .NotRunning:           return "Engine not running."
        case .InvalidDeckIndex:     return "Invalid deck index."
        case .TrackDecodeFailed:    return "Couldn't decode that track."
        case .CommandChannelFull:   return "Audio thread is overloaded — try again."
        case .EngineNotRunning:     return "Engine isn't running — Start it from Preferences (⌘,)."
        }
    }
}

// MARK: - Top-level shell

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
                CommandShortcuts(
                    showingPreferences: $showingPreferences,
                    model: model)
            )
            .onAppear {
                // Auto-open Preferences on first launch when the
                // engine hasn't been configured yet. Avoids the
                // "blank screen, what do I do?" trap.
                if !model.isRunning {
                    showingPreferences = true
                }
            }
    }
}

// MARK: - Keyboard shortcuts

/// Invisible host that registers app-level keyboard shortcuts.
/// `⌘,` toggles Preferences; `Space` loads the FS-browser selection
/// into the non-master stopped deck (PRD §5.5).
///
/// We can't put `.keyboardShortcut` on a `.sheet` modifier directly,
/// so we attach it to invisible `Button` rows that fire the binding.
private struct CommandShortcuts: View {
    @Binding var showingPreferences: Bool
    let model: WaveformAppModel

    var body: some View {
        ZStack {
            Button("Preferences") {
                showingPreferences.toggle()
            }
            .keyboardShortcut(",", modifiers: .command)

            Button("Load selection") {
                model.loadBrowserSelectionIntoNonMaster()
            }
            .keyboardShortcut(.space, modifiers: [])
        }
        .frame(width: 0, height: 0)
        .opacity(0)
        .accessibilityHidden(true)
    }
}

#Preview {
    MainView()
        .frame(width: 1440, height: 900)
}
