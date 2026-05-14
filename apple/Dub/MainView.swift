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

    /// M10.5d. `true` while a `load_track` FFI call is in flight on
    /// this deck (decode + offline-peaks-compute happens on a
    /// background `Task.detached` so the SwiftUI main actor stays
    /// responsive). Drives the deck-header source pill to its
    /// `.loading` variant ("LOADING…", amber dot) and gates
    /// concurrent loads (drag-drop + Space on a loading deck flash
    /// the load-error overlay). Cleared by the model on completion
    /// or error of the dispatched task. Independent from
    /// `hasTrack`: a deck mid-replace-load keeps `hasTrack = true`
    /// (the previous track still plays / renders) while `isLoading
    /// = true`; a cold first load has `hasTrack = false` *and*
    /// `isLoading = true`.
    var isLoading: Bool = false

    /// M10.6c. `true` while the engine is in Panic-Play (PRD
    /// §6.1.2): the deck is decoupled from its timecode input and
    /// running at a held last-known-velocity rate. Driven by the
    /// 30 Hz position poll (`PositionInfo.isPanicPlay`), set / cleared
    /// by `WaveformAppModel.{panic, cancelPanic}(side:)` for an
    /// optimistic round-trip, and auto-cleared by the engine when a
    /// clean LFSR re-lock is detected (PRD §6.1.2 auto-resume).
    /// The deck-header source pill flips to `TC · HOLD` and the
    /// Panic glyph fills while this is `true`; in two-deck Timecode
    /// mode the overview click-jump (PRD §6.1) is allowed only when
    /// this is `true`.
    var isPanicPlay: Bool = false

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
    @Published var palette: WaveformPalette = .serato

    /// Engine mode the next Start call will use. Auto-default
    /// computed at launch; user can override in Preferences.
    @Published var engineMode: EngineMode = .timecode

    // MARK: Live engine state

    @Published private(set) var isRunning: Bool = false
    /// Most recent transient error to surface to the user. Mutated
    /// only via `surfaceError(_:)` so the auto-clear timer stays
    /// consistent. Status-strip + Preferences both read this.
    @Published private(set) var lastError: String? = nil
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

    /// Pending auto-clear task for `lastError`. Cancelled if a new
    /// error supersedes the previous one within the visibility
    /// window.
    private var lastErrorClearTask: Task<Void, Never>?
    private static let errorVisibilitySecs: UInt64 = 5_000_000_000

    // MARK: Init / deinit

    init() {
        self.engine = DubEngine()
        applyAutoDetect()
        // Only enumerate input devices when we actually need them
        // (Timecode mode). Prep mode never touches the input HAL,
        // which is the whole point of the auto-detect — the user
        // never sees a microphone-permission prompt on a Mac with
        // no external interface plugged in.
        if engineMode == .timecode {
            refreshDevices()
        }
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

    /// Pick a default `engineMode` based on what's plugged in.
    ///
    /// **Permission-safe.** Uses [`DubEngine.hasExternalAudioInterface`]
    /// which queries CoreAudio transport-type metadata only — no
    /// AudioUnit instantiation, no device-name reads on input-
    /// capable devices, nothing that would tickle macOS's
    /// microphone-permission TCC layer. PRD §3.1: external
    /// interface present → Performance / Timecode; none present →
    /// Track Preparation / output-only (no input touched at all).
    ///
    /// "External" here is defined by transport type — USB,
    /// Thunderbolt, FireWire, PCI, AVB — i.e. the bus types DVS
    /// interfaces actually use. The previous heuristic (string-
    /// match device names against built-in-mic patterns) called
    /// `listInputDevices` which itself triggered the TCC prompt on
    /// macOS 14+; that was the regression the user reported in
    /// M10.5b shakedown.
    private func applyAutoDetect() {
        engineMode = engine.hasExternalAudioInterface() ? .timecode : .prep
    }

    // MARK: Engine lifecycle

    /// Apply the current Preferences config to the engine — start
    /// it if stopped, restart it if running. This is the single
    /// engine-lifecycle entry point used everywhere in M10.5b:
    /// `MainView.onAppear` calls it for the cold-boot auto-start,
    /// and every Preferences `onChange` (mode / device / channels)
    /// calls it so the new config takes effect with zero clicks.
    ///
    /// Use `stop()` for the explicit user-stop path. Don't call
    /// `start()` directly anymore — `applyConfig` is the only
    /// caller that knows whether a restart-vs-fresh-start is needed.
    func applyConfig() {
        // Just-in-time device enumeration. The auto-detect at init
        // *intentionally* skipped `refreshDevices()` when Prep mode
        // was picked, so the user never saw the mic-permission prompt
        // on a Mac with no external interface. The moment the user
        // (or some onChange handler) selects Timecode mode, we need
        // a device list — call `refreshDevices()` here so the
        // Preferences picker has something to show. This is the
        // explicit-user-action point where macOS's TCC prompt may
        // fire, and that's the right time for it.
        if engineMode == .timecode && availableDevices.isEmpty {
            refreshDevices()
        }
        let wasRunning = isRunning
        if wasRunning {
            stop()
        }
        start()
    }

    func start() {
        surfaceError(nil)
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
            surfaceError("Pick an input device first.")
            return
        }
        let channelsA: [UInt32]
        switch parseChannels(channelsAText, side: "A") {
        case .success(let cs): channelsA = cs
        case .failure(let msg):
            surfaceError(msg)
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
                    surfaceError(msg)
                    return
                }
                try engine.startThruTwoDeck(
                    deviceName: device, channelsA: channelsA, channelsB: channelsB)
                twoDeckMode = true
            }
            isRunning = true
            masterDeck = stickyMaster
        } catch let error as EngineError {
            surfaceError(describe(error))
        } catch {
            surfaceError("Unexpected error: \(error.localizedDescription)")
        }
    }

    private func startPrep() {
        do {
            try engine.startEngine(outputChannels: 2)
            isRunning = true
            twoDeckMode = false
            masterDeck = stickyMaster
        } catch let error as EngineError {
            surfaceError(describe(error))
        } catch {
            surfaceError("Unexpected error: \(error.localizedDescription)")
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
        next.isPanicPlay = pos.isPanicPlay
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
        // Single-deck modes (Prep, single-channel Timecode) only
        // ever have deck A. Pinning master to .a keeps the MASTER
        // chip stable and stops the non-master Space-load logic
        // from ever picking the non-existent deck B.
        guard twoDeckMode else {
            if masterDeck != .a { masterDeck = .a }
            stickyMaster = .a
            return
        }
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

    /// Load a track onto `side` (M10.5d background-load).
    ///
    /// Refuses (and red-flashes the deck pane) if the target deck
    /// is currently playing (PRD §5.5 + §6.4 — the user must lift
    /// the needle / pause first) **or** if another load is already
    /// in flight on the same deck (avoids racing two decoders
    /// against each other and stomping the deck's `Arc<Track>`).
    ///
    /// **Concurrency.** `engine.loadTrack` is the Rust FFI, which
    /// in M10.5d does its decode + offline-peaks compute outside
    /// the engine-state mutex (see `dub-ffi` `load_track` docs).
    /// We wrap the FFI call in `Task.detached` so it runs off the
    /// SwiftUI main actor — the 30 Hz position poll + waveform
    /// rendering both stay responsive throughout. Returns `true`
    /// on success.
    ///
    /// **Optimistic UI.** Title + format chip flip to the *new*
    /// file before decode starts (so the deck immediately reads
    /// "Loading… MyTrack.mp3"); duration / has-track land once the
    /// FFI call returns. If a previous track was loaded, its
    /// waveform stays visible until the new peaks arrive — the
    /// renderer's `peaksGeneration` mismatch handler resets the
    /// view at the moment of swap.
    @discardableResult
    func loadTrack(side: DeckSide, url: URL) async -> Bool {
        guard isRunning else {
            surfaceError("Engine not running. Open Preferences (⌘,) and Start.")
            return false
        }
        let target = state(for: side)
        if target.isPlaying {
            flashLoadError(side: side)
            return false
        }
        if target.isLoading {
            flashLoadError(side: side)
            surfaceError("Deck \(side.label) is already loading a track. Wait or load onto the other deck.")
            return false
        }

        // Optimistic UI: header pill flips to LOADING + new title
        // appears before the decode work starts.
        var starting = target
        starting.isLoading = true
        starting.sourceURL = url
        starting.displayName = url.deletingPathExtension().lastPathComponent
        starting.errorFlashUntil = nil
        setState(starting, for: side)

        let deckIdx = side.ffiDeckIdx
        let engineRef = engine
        let result: Result<Void, Error> = await Task.detached(priority: .userInitiated) {
            do {
                try engineRef.loadTrack(deckIdx: deckIdx, path: url.path)
                return .success(())
            } catch {
                return .failure(error)
            }
        }.value

        switch result {
        case .success:
            var next = state(for: side)
            next.hasTrack = true
            next.atEnd = false
            next.isPlaying = false
            next.elapsedSecs = 0
            next.remainingSecs = 0
            next.isLoading = false
            if let info = engine.trackInfo(deckIdx: deckIdx) {
                next.durationSecs = info.durationSecs
                next.formatChip = formatChip(for: url, info: info)
            }
            setState(next, for: side)
            recomputeMaster()
            return true
        case .failure(let error):
            var failed = state(for: side)
            failed.isLoading = false
            setState(failed, for: side)
            if let engineError = error as? EngineError {
                surfaceError(describe(engineError))
            } else {
                surfaceError("Unexpected load error: \(error.localizedDescription)")
            }
            return false
        }
    }

    /// Load the FS-browser selection into the appropriate target
    /// deck. PRD §5.5 — bound to `Space` in `MainView`.
    ///
    /// Target deck selection:
    /// * Two-deck (Timecode + non-empty deck-B channels) → the
    ///   non-master deck.
    /// * Single-deck (Timecode single-channel **or** Prep) → deck
    ///   A. Prep mode by definition has no deck B, and single-
    ///   channel Timecode never spins one up, so "non-master" isn't
    ///   meaningful and Space loads onto the only deck that exists.
    func loadBrowserSelectionIntoTargetDeck() async {
        guard isRunning else {
            surfaceError("Engine not running.")
            return
        }
        guard let url = browserSelection else {
            surfaceError("Select a file in the browser first.")
            return
        }
        // Single-click in the browser now selects folders too (so
        // the highlight follows keyboard intuition) — but Space
        // shouldn't try to load a folder as audio. Skip with a
        // polite hint instead of letting the FFI return a decode
        // error.
        var isDir: ObjCBool = false
        if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir),
           isDir.boolValue {
            surfaceError("Selected entry is a folder — double-click it to enter, or pick an audio file inside.")
            return
        }
        let candidate = spaceLoadTarget()
        let target = state(for: candidate)
        if target.isPlaying {
            flashLoadError(side: candidate)
            return
        }
        _ = await loadTrack(side: candidate, url: url)
    }

    /// The deck Space-load targets in the current engine config.
    /// See `loadBrowserSelectionIntoTargetDeck` for the rules.
    private func spaceLoadTarget() -> DeckSide {
        guard twoDeckMode else { return .a }
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
            surfaceError(describe(error))
        } catch {
            surfaceError("Play failed: \(error.localizedDescription)")
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
            surfaceError(describe(error))
        } catch {
            surfaceError("Pause failed: \(error.localizedDescription)")
        }
    }

    /// M10.6a Casual-Play "Restart" (PRD §6.1.3). Seeks the deck to
    /// 0:00 and resumes playback. No-op if the engine isn't running
    /// or the deck has no track loaded. Mirror of `play(side:)` for
    /// error handling + master recomputation.
    func restart(side: DeckSide) {
        guard isRunning else { return }
        let deck = state(for: side)
        guard deck.hasTrack else { return }
        do {
            try engine.seek(deckIdx: side.ffiDeckIdx, positionSecs: 0)
            try engine.play(deckIdx: side.ffiDeckIdx)
            var s = state(for: side)
            s.elapsedSecs = 0
            s.atEnd = false
            s.isPlaying = true
            setState(s, for: side)
            lastPlayStart[side] = Date()
            recomputeMaster()
        } catch let error as EngineError {
            surfaceError(describe(error))
        } catch {
            surfaceError("Restart failed: \(error.localizedDescription)")
        }
    }

    /// M10.6a zoomed click-scrub (PRD §6.1). Given a signed offset
    /// in seconds relative to the current playhead, clamp into the
    /// track's `[0, durationSecs]` range and seek the engine there.
    /// `WaveformView` only invokes this when the parent
    /// `PerformanceView` opts in (Prep mode in M10.6a; Timecode-mode
    /// click-scrub is intentionally disabled per the PRD).
    func scrub(side: DeckSide, relativeSecs: TimeInterval) {
        guard isRunning else { return }
        let deck = state(for: side)
        guard deck.hasTrack, deck.durationSecs > 0 else { return }
        let target = max(0, min(deck.durationSecs, deck.elapsedSecs + relativeSecs))
        do {
            try engine.seek(deckIdx: side.ffiDeckIdx, positionSecs: target)
            var s = state(for: side)
            s.elapsedSecs = target
            s.atEnd = false
            setState(s, for: side)
        } catch let error as EngineError {
            surfaceError(describe(error))
        } catch {
            surfaceError("Scrub failed: \(error.localizedDescription)")
        }
    }

    // MARK: Panic Play (PRD §6.1.2 / M10.6c)

    /// Engage Panic Play on `side`. Engine decouples the deck from
    /// its timecode input and holds the last-known forward velocity
    /// (M10.6b engine logic). UI-side we set `isPanicPlay` optimistically
    /// so the deck header pill / glyph flip without waiting for the
    /// next 30 Hz poll round-trip; the poll then keeps the field
    /// authoritative (in particular, picking up an engine-side
    /// auto-cancel on clean LFSR re-lock).
    ///
    /// No-op if the engine isn't running or the deck has no track —
    /// Panic Play needs audible audio to recover *to*. The deck-header
    /// glyph is gated to the same conditions, so this is mostly a
    /// defence-in-depth check.
    func panic(side: DeckSide) {
        guard isRunning else { return }
        let deck = state(for: side)
        guard deck.hasTrack else { return }
        do {
            try engine.panicPlay(deckIdx: side.ffiDeckIdx)
            var s = state(for: side)
            s.isPanicPlay = true
            s.isPlaying = true
            setState(s, for: side)
            lastPlayStart[side] = Date()
            recomputeMaster()
        } catch let error as EngineError {
            surfaceError(describe(error))
        } catch {
            surfaceError("Panic Play failed: \(error.localizedDescription)")
        }
    }

    /// Cancel Panic Play on `side` — Serato INT→ABS toggle.
    ///
    /// PRD §6.1.2 / M10.6d: the engine clears its panic flag but
    /// *does not* touch deck transport. The next render block lets
    /// the timecode driver re-engage on a healthy carrier (deck
    /// keeps playing) or pause the deck via the existing
    /// `DropoutHoldRate` arm on a silent / broken cartridge. We
    /// mirror that here: clear `isPanicPlay` optimistically but
    /// leave `isPlaying` alone — the 30 Hz poll will read whatever
    /// the engine decides ≤33 ms from now.
    func cancelPanic(side: DeckSide) {
        guard isRunning else { return }
        do {
            try engine.cancelPanicPlay(deckIdx: side.ffiDeckIdx)
            var s = state(for: side)
            s.isPanicPlay = false
            setState(s, for: side)
            recomputeMaster()
        } catch let error as EngineError {
            surfaceError(describe(error))
        } catch {
            surfaceError("Cancel Panic failed: \(error.localizedDescription)")
        }
    }

    /// Timecode-mode primary-button toggle (M10.6d UI redesign).
    /// Mirrors Serato's INT/ABS button: tap once to switch from
    /// platter-driven playback to internal (panic engaged), tap
    /// again to switch back. The deck-header transport button uses
    /// this directly when `engineMode == .timecode` and a track is
    /// loaded; Prep mode still routes through `play` / `pause`.
    func panicToggle(side: DeckSide) {
        if state(for: side).isPanicPlay {
            cancelPanic(side: side)
        } else {
            panic(side: side)
        }
    }

    // MARK: Helpers

    /// Single sink for surfaceable user-facing errors. Updates
    /// `lastError` and schedules a `Task` to clear it after
    /// `errorVisibilitySecs`, cancelling any prior pending clear.
    /// Passing `nil` clears immediately.
    func surfaceError(_ message: String?) {
        lastErrorClearTask?.cancel()
        lastErrorClearTask = nil
        lastError = message
        guard message != nil else { return }
        let task = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: Self.errorVisibilitySecs)
            guard let self else { return }
            if !Task.isCancelled {
                self.lastError = nil
            }
        }
        lastErrorClearTask = task
    }

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
/// Preferences sheet.
struct MainView: View {

    @StateObject private var model = WaveformAppModel()
    @State private var showingPreferences: Bool = false

    var body: some View {
        PerformanceView(model: model, openPreferences: { showingPreferences = true })
            .frame(minWidth: 960, minHeight: 600)
            .sheet(isPresented: $showingPreferences) {
                PreferencesSheet(model: model)
            }
            .background(
                KeyEventMonitorHost(
                    showingPreferences: $showingPreferences,
                    model: model)
            )
            // M10.5b "no Apply button" UX: every Preferences-driven
            // config change auto-applies. `applyConfig()` starts the
            // engine when stopped and restarts it when running, so
            // the user only ever needs to *change* a setting; the
            // engine catches up on its own.
            .onChange(of: model.engineMode) { _ in
                model.applyConfig()
            }
            .onChange(of: model.selectedDevice) { _ in
                model.applyConfig()
            }
            .onAppear {
                // Cold-boot auto-start: if a valid config exists for
                // the auto-detected mode (Prep always works; Timecode
                // works as long as `selectedDevice` is set), spin up
                // the engine. If start fails (no device + Timecode
                // selected), `surfaceError` will display the reason
                // in the status strip and the user can open
                // Preferences from the gear icon to fix it.
                if !model.isRunning {
                    model.applyConfig()
                }
            }
    }
}

// MARK: - Keyboard event monitor

/// Hidden NSView host that installs an `NSEvent.addLocalMonitorForEvents`
/// handler at view-mount. Keyboard shortcuts placed on SwiftUI
/// `Button`s with `.opacity(0)` are unreliable — when a child view
/// (the FileBrowserView's scroll-view, a TextField, etc.) holds
/// keyboard focus, the synthetic Button doesn't fire. NSEvent's
/// local monitor intercepts every keyDown delivered to the
/// application before any first responder gets it, which is the
/// only way to make `Space` work the way `⌘,` does in macOS.
private struct KeyEventMonitorHost: NSViewRepresentable {
    @Binding var showingPreferences: Bool
    let model: WaveformAppModel

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeNSView(context: Context) -> NSView {
        let view = NSView(frame: .zero)
        context.coordinator.install(
            onSpace: {
                Task { @MainActor in
                    await model.loadBrowserSelectionIntoTargetDeck()
                }
                return true
            },
            onCmdComma: {
                Task { @MainActor in
                    showingPreferences.toggle()
                }
                return true
            })
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        // Bindings are captured by reference; no per-update work
        // required — the monitor stays installed for the
        // coordinator's lifetime.
    }

    static func dismantleNSView(_ nsView: NSView, coordinator: Coordinator) {
        coordinator.uninstall()
    }

    @MainActor
    final class Coordinator {
        private var monitor: Any?

        func install(
            onSpace: @escaping () -> Bool,
            onCmdComma: @escaping () -> Bool
        ) {
            uninstall()
            monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
                guard let self else { return event }
                let isCmd = event.modifierFlags.contains(.command)
                if isCmd, event.charactersIgnoringModifiers == "," {
                    if onCmdComma() { return nil }
                    return event
                }
                // Don't intercept Space while the user is typing
                // into a TextField (Preferences channel fields,
                // future search boxes, etc.). `⌘,` is a global
                // shortcut so it always wins.
                if self.isTextFirstResponder() {
                    return event
                }
                // `keyCode 49` is the spacebar on every Apple keyboard
                // layout (the keyCodes are layout-independent for the
                // physical-key tier of NSEvent).
                if !isCmd, event.keyCode == 49 {
                    if onSpace() { return nil }
                }
                return event
            }
        }

        func uninstall() {
            if let m = monitor { NSEvent.removeMonitor(m) }
            monitor = nil
        }

        private func isTextFirstResponder() -> Bool {
            guard let responder = NSApp.keyWindow?.firstResponder else {
                return false
            }
            return responder is NSText || responder is NSTextView
        }

        deinit {
            if let m = monitor { NSEvent.removeMonitor(m) }
        }
    }
}

#Preview {
    MainView()
        .frame(width: 1440, height: 900)
}
