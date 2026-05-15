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

    /// Filename stem of the loaded track. Used as the deck-header
    /// title fallback when the container has no ID3 / Vorbis /
    /// MP4 title tag.
    var displayName: String? = nil

    /// Track title parsed from the container's tag block (M10.5r).
    /// `nil` when the file is untagged — the deck header falls
    /// back to `displayName`.
    var trackTitle: String? = nil

    /// Track artist parsed from the container's tag block (M10.5r).
    /// `nil` when the file is untagged.
    var trackArtist: String? = nil

    /// Format / SR chip ("MP3 · 44.1 kHz · 2 ch"). `nil` until a
    /// track loads.
    var formatChip: String? = nil

    /// Total track duration. 0 if no track loaded.
    var durationSecs: Double = 0

    /// Wall-clock elapsed from track start. 0 if no track.
    /// Clamped to `[0, durationSecs]` so it's safe to use in time
    /// displays and as a seek target. The waveform renderer uses
    /// `playheadSecsUnclamped` instead so the playhead can drift
    /// into the lead-in / lead-out empty groove during a hard
    /// scratch off either edge (PRD §6.1 / §9.6).
    var elapsedSecs: Double = 0

    /// M10.5t. Raw playhead position in seconds, NOT clamped to
    /// `[0, durationSecs]`. Goes negative when a mouse-scratch
    /// has pushed the deck backwards past `t = 0`, and exceeds
    /// `durationSecs` when it has been pushed past the end. The
    /// audio thread already renders silence outside the track's
    /// frame range; this field lets the waveform render the
    /// playhead in the empty-groove region instead of pinning it
    /// to the nearest edge. **Only the renderer should read it.**
    /// Time-display consumers stay on `elapsedSecs`.
    var playheadSecsUnclamped: Double = 0

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

    /// Allow loading a track onto a *playing* deck while in
    /// Performance / Timecode mode. The PRD's default policy (§5.5,
    /// §6.4) is "no — the DJ must lift the needle / pause first",
    /// surfaced as a 200 ms red flash on the rejected pane. Some
    /// users want the rule relaxed (e.g. they're rehearsing
    /// transitions and want to drop a new file mid-play without
    /// pausing first). This toggle lets them opt out of the safety
    /// rule. **Prep mode always allows it** regardless of this
    /// setting — Prep is a single-deck rehearsal shell where the
    /// "deck is playing in front of a crowd" concern doesn't apply.
    ///
    /// Persisted in `UserDefaults` under
    /// `dub.allowLoadIntoRunningDeckInPerformance`. The setting
    /// applies on the next load attempt; in-flight loads are not
    /// retroactively affected.
    @Published var allowLoadIntoRunningDeckInPerformance: Bool {
        didSet {
            UserDefaults.standard.set(
                allowLoadIntoRunningDeckInPerformance,
                forKey: Self.kAllowLoadIntoRunningDeck)
        }
    }

    private static let kAllowLoadIntoRunningDeck = "dub.allowLoadIntoRunningDeckInPerformance"

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
        self.allowLoadIntoRunningDeckInPerformance =
            UserDefaults.standard.bool(forKey: Self.kAllowLoadIntoRunningDeck)
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
        next.playheadSecsUnclamped = pos.playheadSecsUnclamped
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
    /// **Refuses (and red-flashes the deck pane) when** the target
    /// deck is currently playing **and** the load-into-playing-deck
    /// guard is active (PRD §5.5 + §6.4 — the user must lift the
    /// needle / pause first). M10.5r relaxed the guard:
    ///
    /// * Prep mode always allows the load — Prep is a single-deck
    ///   rehearsal shell, not a stage workflow.
    /// * Performance / Timecode mode respects
    ///   `allowLoadIntoRunningDeckInPerformance` — the user opts in
    ///   from Preferences if they want to drop tracks mid-play.
    ///
    /// Also refuses when another load is already in flight on the
    /// same deck (avoids racing two decoders against each other
    /// and stomping the deck's `Arc<Track>`).
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
        if target.isPlaying, !canLoadIntoPlayingDeck() {
            flashLoadError(side: side)
            return false
        }
        if target.isLoading {
            flashLoadError(side: side)
            surfaceError("Deck \(side.label) is already loading a track. Wait or load onto the other deck.")
            return false
        }

        // Optimistic UI: header pill flips to LOADING + new file
        // basename appears before the decode work starts. We clear
        // the old tag-derived title / artist so the header doesn't
        // show stale metadata from the previous track during the
        // ~50 ms decode window.
        var starting = target
        starting.isLoading = true
        starting.sourceURL = url
        starting.displayName = url.deletingPathExtension().lastPathComponent
        starting.trackTitle = nil
        starting.trackArtist = nil
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
                next.trackTitle = info.title.isEmpty ? nil : info.title
                next.trackArtist = info.artist.isEmpty ? nil : info.artist
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
        if target.isPlaying, !canLoadIntoPlayingDeck() {
            flashLoadError(side: candidate)
            return
        }
        _ = await loadTrack(side: candidate, url: url)
    }

    /// `true` when a load is allowed to land on a deck that is
    /// currently playing. See `loadTrack(side:url:)` for the policy:
    /// Prep mode always allows; Performance mode checks
    /// `allowLoadIntoRunningDeckInPerformance`.
    private func canLoadIntoPlayingDeck() -> Bool {
        switch engineMode {
        case .prep:     return true
        case .timecode: return allowLoadIntoRunningDeckInPerformance
        }
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
        seekDeck(side: side, absoluteSecs: target)
    }

    // MARK: - Vinyl-style mouse scratch (M10.5s)
    //
    // PRD §1 / §6.1 — the zoomed-waveform drag is a *scratch*
    // gesture: audio plays only while the mouse is moving, and only
    // at the rate the mouse is moving (left = reverse, right /
    // down = forward). Mouse-still ⇒ silence, identical to a record
    // sitting under a stationary stylus. The previous M10.5r
    // implementation was a seek-and-play loop that ran the deck at
    // 1× under the cursor — that violated the "feels like a
    // turntable" expectation and is gone as of M10.5s.
    //
    // Implementation:
    //
    //   1. `scratchBegin(side:)` — capture pre-scratch transport.
    //      In Timecode mode, engage Panic Play so the timecode
    //      driver doesn't fight `setDeckRate` every block. Pin
    //      `is_playing = true` (so the audio thread renders the
    //      deck), `rate = 0` (so the playhead is frozen until the
    //      first move). Spin up a 60 Hz polling timer.
    //   2. `scratchPointerOffset(side:offsetSecs:)` — the view
    //      reports the cursor's running offset (in audio seconds)
    //      from the drag's start point. The timer reads this
    //      between ticks; nothing else.
    //   3. Timer tick — compute `rate = Δoffset / Δrealtime` since
    //      the previous tick and `setDeckRate(rate)`. When the
    //      mouse is held still both deltas collapse to ~0 and the
    //      deck reads the same sample frame block-after-block,
    //      which the platter de-click in the engine smooths to
    //      silence.
    //   4. `scratchEnd(side:)` — stop the timer, cancel Panic Play
    //      (if we engaged it), restore the pre-scratch transport
    //      (rate = 1.0, set_playing to whatever it was before).
    //
    // **Position drift.** We deliberately don't seek during the
    // drag — the engine's own playhead integration accumulates
    // `rate × block_size` per block, so position naturally tracks
    // the cursor. Seeking every tick would fire a 2 ms de-click
    // on every block (`set_position_frames` ramps amplitude to
    // zero and back), turning the scratch into a tremolo. Drift
    // is bounded by the rate-conversion accuracy of
    // `set_deck_rate`'s SR math; in practice it's <10 ms over a
    // multi-second scratch, well below the visual resolution of
    // the waveform.

    /// Per-deck scratch state. `nil` ⇒ no scratch in flight on
    /// that side. Stored as a class so the polling timer's `[weak
    /// self]` closure doesn't need to chase a per-side enum case
    /// on every tick.
    ///
    /// M10.5t rework: the rate is now derived **per-event** in
    /// `scratchPointerOffset(side:offsetSecs:)` from the elapsed
    /// real-time since the previous event, then low-pass filtered
    /// with an exponential moving average. The old 60 Hz polling
    /// timer's aliasing (sampling a high-rate event stream at a
    /// fixed cadence produced periodic rate spikes that read as
    /// audible "jumping" — confirmed in pre-M10.5t dogfood) is
    /// gone. The timer that remains is a low-rate watchdog whose
    /// only job is to ramp the rate to zero when the cursor
    /// stops moving (no `onChanged` event for > stallThresholdSecs)
    /// so a stationary mouse plays silence like a stationary
    /// platter.
    private final class ScratchState {
        let side: DeckSide
        let priorIsPlaying: Bool
        let engagedPanic: Bool
        /// Most recent cursor offset (in audio seconds) reported
        /// by the gesture overlay. Reset to 0 on begin.
        var lastEventOffsetSecs: Double = 0
        /// Wall-clock time of the most recent `scratchPointerOffset`
        /// call. Used both to compute `Δt` for the per-event rate
        /// and by the watchdog to detect "cursor still".
        var lastEventAt: Date
        /// Smoothed instantaneous rate. Updated by each gesture
        /// event and ramped toward zero by the watchdog timer
        /// when no event fires for a while. Cached so the
        /// watchdog doesn't have to round-trip through the engine
        /// to check what rate is currently in flight.
        var smoothedRate: Double = 0

        init(side: DeckSide, priorIsPlaying: Bool, engagedPanic: Bool, startedAt: Date) {
            self.side = side
            self.priorIsPlaying = priorIsPlaying
            self.engagedPanic = engagedPanic
            self.lastEventAt = startedAt
        }
    }

    /// In-flight scratch per deck. Keyed by side so deck A and B
    /// can be scratched independently (rare in practice — the
    /// user has one mouse — but the model doesn't enforce
    /// exclusivity, the view layer does).
    private var scratchStates: [DeckSide: ScratchState] = [:]

    /// Watchdog timer that ramps the rate toward zero on each
    /// deck whose cursor has been still for longer than
    /// `scratchStallThresholdSecs`. Runs only while ≥ 1 scratch
    /// is in flight; lazily torn down by `scratchEnd`.
    private var scratchTimer: Timer?
    /// Watchdog fires at this cadence. Must be << the typical
    /// gesture event rate so we don't fight the per-event rate
    /// path on a steady drag, but fast enough that "cursor held
    /// still after a fast scratch" responds within one perceptual
    /// frame.
    private static let scratchTickIntervalSecs: TimeInterval = 1.0 / 60.0
    /// If no `scratchPointerOffset` event has fired within this
    /// window, the watchdog treats the cursor as "still" and
    /// ramps the deck's rate toward zero. 25 ms is comfortably
    /// longer than the inter-arrival time of a smooth drag
    /// (≈ 8–17 ms on a 60–120 Hz event stream) so a normal drag
    /// never trips it, but short enough that letting go of a
    /// pushed scratch produces immediate silence rather than
    /// drifting at the last-seen velocity.
    private static let scratchStallThresholdSecs: TimeInterval = 0.025
    /// Per-event EMA factor applied to the instantaneous rate.
    /// `0.35` was chosen empirically: high enough that fast
    /// direction changes still feel direct (one event lands ~⅓ of
    /// the new direction in the output rate), low enough that
    /// single-event outliers from coalesced or jittered cursor
    /// motion don't get punched through into the engine. Lower
    /// values feel mushy / lagged; higher values reintroduce the
    /// pre-M10.5t "jumping".
    private static let scratchRateEMAAlpha: Double = 0.35
    /// Multiplicative decay applied to the smoothed rate on each
    /// watchdog tick when the cursor is still. Picked so the rate
    /// halves in ~3 ticks (≈ 50 ms): fast enough to read as
    /// "let go of the platter" but smooth enough that the engine
    /// doesn't see a discontinuity that the platter de-click
    /// might otherwise punch through to the speakers.
    private static let scratchRateStallDecay: Double = 0.7

    /// Begin a vinyl-style scratch on `side`. Captures the pre-
    /// scratch transport, engages Panic Play (Timecode mode only),
    /// freezes the playhead via `rate = 0` + `playing = true`, and
    /// spins up the rate-from-velocity polling timer.
    ///
    /// Idempotent on a deck that's already scratching — the second
    /// begin is a no-op so the lazy-begin pattern in the gesture
    /// overlay (begin on every `onChanged` until we see one)
    /// doesn't clobber the captured prior state.
    func scratchBegin(side: DeckSide) {
        guard isRunning else { return }
        let deck = state(for: side)
        guard deck.hasTrack else { return }
        if scratchStates[side] != nil { return }

        let prior = deck.isPlaying
        var engagedPanic = false
        if engineMode == .timecode && !deck.isPanicPlay {
            // Decouple from the timecode driver so our setDeckRate
            // sticks. `panic` updates `isPanicPlay` optimistically.
            panic(side: side)
            engagedPanic = true
        }

        do {
            try engine.setDeckRate(deckIdx: side.ffiDeckIdx, rate: 0.0)
        } catch {
            surfaceError("Scratch start failed: \(error.localizedDescription)")
            return
        }

        if !state(for: side).isPlaying {
            do {
                try engine.play(deckIdx: side.ffiDeckIdx)
                var s = state(for: side)
                s.isPlaying = true
                setState(s, for: side)
            } catch {
                surfaceError("Scratch start failed: \(error.localizedDescription)")
            }
        }

        scratchStates[side] = ScratchState(
            side: side,
            priorIsPlaying: prior,
            engagedPanic: engagedPanic,
            startedAt: Date())
        ensureScratchTimerRunning()
    }

    /// Report the mouse cursor's running offset (in audio seconds)
    /// from the scratch's start point. Positive = forward; negative
    /// = reverse. Each call drives an immediate per-event rate
    /// update so the engine never sees a 60 Hz-aliased velocity
    /// (M10.5t — pre-rework this lived in `scratchTick` and produced
    /// audible "jumping" when the event stream and the tick clock
    /// beat against each other).
    func scratchPointerOffset(side: DeckSide, offsetSecs: Double) {
        guard let state = scratchStates[side] else { return }
        let now = Date()
        let dt = now.timeIntervalSince(state.lastEventAt)
        let delta = offsetSecs - state.lastEventOffsetSecs
        state.lastEventOffsetSecs = offsetSecs
        state.lastEventAt = now

        // First event after begin has `delta == 0` (offsetSecs is 0
        // by definition at the drag's start). Skip the rate update
        // — using `dt` here would compute a zero rate based on a
        // potentially long gap since scratchBegin, and using the
        // raw `delta` would compute a meaningless rate from an
        // artificial zero baseline. The next event provides the
        // first real velocity sample.
        guard dt > 0, abs(delta) > 0 else { return }

        // Reject impossible inter-event gaps. macOS occasionally
        // batches multiple cursor events into one onChanged
        // callback after a stall (e.g. the app was descheduled);
        // those produce a huge delta over a near-zero dt that
        // would saturate the rate clamp at ±8× on a single sample.
        // Treat dt < 1 ms as "coalesced event" and use the most
        // recent realistic dt instead.
        let effectiveDt = max(dt, 0.001)
        let instantRate = delta / effectiveDt
        // EMA smoothing — see `scratchRateEMAAlpha` rationale.
        let alpha = Self.scratchRateEMAAlpha
        let smoothed =
            alpha * instantRate + (1.0 - alpha) * state.smoothedRate
        // Clamp to a sane range so a glitched event burst doesn't
        // send the playhead off to lunch. ±8× is the upper bound
        // a turntablist would ever hand-spin a platter at; the
        // engine itself accepts wider but the resampler quality
        // falls off past ~4×.
        let clamped = max(-8.0, min(8.0, smoothed))
        state.smoothedRate = clamped
        try? engine.setDeckRate(
            deckIdx: side.ffiDeckIdx,
            rate: clamped)
    }

    /// End an in-flight scratch on `side`. Stops the watchdog
    /// timer for this deck, sets rate back to 1.0, restores the
    /// pre-scratch play / pause state, and cancels Panic Play if
    /// we engaged it on `scratchBegin`. No-op on a side that
    /// isn't currently scratching.
    func scratchEnd(side: DeckSide) {
        guard let state = scratchStates.removeValue(forKey: side) else { return }
        // Restore the deck rate first so the brief window between
        // here and the play / pause switch below renders at unity.
        try? engine.setDeckRate(deckIdx: side.ffiDeckIdx, rate: 1.0)
        if state.engagedPanic {
            cancelPanic(side: side)
        }
        if !state.priorIsPlaying {
            pause(side: side)
        }
        if scratchStates.isEmpty {
            scratchTimer?.invalidate()
            scratchTimer = nil
        }
    }

    private func ensureScratchTimerRunning() {
        if scratchTimer != nil { return }
        let timer = Timer.scheduledTimer(
            withTimeInterval: Self.scratchTickIntervalSecs, repeats: true
        ) { [weak self] _ in
            Task { @MainActor [weak self] in self?.scratchTick() }
        }
        // No tolerance — the watchdog catches cursor-still windows
        // and needs predictable cadence to ramp the rate down on
        // a known schedule.
        RunLoop.main.add(timer, forMode: .common)
        scratchTimer = timer
    }

    private func scratchTick() {
        guard !scratchStates.isEmpty else {
            scratchTimer?.invalidate()
            scratchTimer = nil
            return
        }
        let now = Date()
        for (_, state) in scratchStates {
            let stalledFor = now.timeIntervalSince(state.lastEventAt)
            guard stalledFor > Self.scratchStallThresholdSecs else { continue }
            // Cursor is still — ramp the rate toward zero. The
            // multiplicative decay produces a brief audible
            // run-out (matching how a real platter coasts after
            // the DJ lifts their finger) rather than slamming to
            // a hard zero, which the audio thread's own platter
            // de-click would otherwise need to absorb in one
            // block.
            if state.smoothedRate == 0 { continue }
            var next = state.smoothedRate * Self.scratchRateStallDecay
            if abs(next) < 0.01 { next = 0 }
            state.smoothedRate = next
            try? engine.setDeckRate(
                deckIdx: state.side.ffiDeckIdx,
                rate: next)
        }
    }

    /// Shared seek + optimistic UI update. Used by the overview's
    /// click-to-jump (PRD §6.1) and the Casual-Play restart path.
    /// Surfaces engine errors in the status strip rather than
    /// throwing.
    func seekDeck(side: DeckSide, absoluteSecs: TimeInterval) {
        guard isRunning else { return }
        let deck = state(for: side)
        guard deck.hasTrack, deck.durationSecs > 0 else { return }
        let clamped = max(0, min(deck.durationSecs, absoluteSecs))
        do {
            try engine.seek(deckIdx: side.ffiDeckIdx, positionSecs: clamped)
            var s = state(for: side)
            s.elapsedSecs = clamped
            s.remainingSecs = max(0, s.durationSecs - clamped)
            s.atEnd = false
            setState(s, for: side)
        } catch let error as EngineError {
            surfaceError(describe(error))
        } catch {
            surfaceError("Seek failed: \(error.localizedDescription)")
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
