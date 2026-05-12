//
//  WaveformRenderer.swift
//  Dub
//
//  M10-B Metal renderer for the broadband waveform.
//
//  Owns:
//      • `MTLDevice` + `MTLCommandQueue`
//      • render pipeline state (vertex + fragment from `Shaders.metal`)
//      • two uniforms buffers (double-buffered against in-flight draws)
//      • one large append-only `chunks` buffer holding the most recent
//        `chunkCapacity` `PeakChunk`s as a power-of-two ring
//
//  Polls the Rust engine each frame via `DubEngine.peaksExtend(...)`.
//  No callbacks from the audio thread — the renderer is purely a
//  consumer of the M9 peak buffer.
//
//  Threading: All renderer work runs on the main thread. `MTKView`
//  invokes `draw(in:)` on the main thread when isPaused == false and
//  enableSetNeedsDisplay == false (our configuration in `WaveformView`).
//  The `DispatchSemaphore` blocks the CPU only when more than
//  `maxFramesInFlight` frames are queued — never a deadlock against
//  the audio thread.

import Foundation
import Metal
import MetalKit
import simd

import DubCore

/// CPU-side mirror of `Shaders.metal`'s `Uniforms` struct. Field
/// order, type, and padding match exactly so we can `memcpy` it into
/// the uniforms buffer without any per-frame allocation.
///
/// Nine 4-byte fields = 36 bytes total. Padded out to 40 bytes by
/// Metal's natural alignment for a `constant Uniforms&`.
private struct WaveformUniforms {
    var chunkOffset: UInt32
    var chunksVisible: UInt32
    /// M10.5b: number of `chunksVisible` instances assigned to the
    /// past region (top 25 %). When `chunksAbovePlayhead ==
    /// chunksVisible` the renderer behaves as in M10.4 (past only,
    /// no future). When `chunksAbovePlayhead < chunksVisible` the
    /// remaining instances render in the future region (bottom 75 %).
    /// See Shaders.metal layout doc.
    var chunksAbovePlayhead: UInt32
    var yScale: Float
    var samplesPerPeakChunk: UInt32
    var bandChunkOffset: UInt32
    var samplesPerBandChunk: UInt32
    var bandCapacity: UInt32
    var palette: UInt32 = 0
}

/// M10.2 palette presets. Matches the `palette` uint forwarded into
/// `Shaders.metal` via the uniforms struct.
public enum WaveformPalette: UInt32, CaseIterable, Identifiable {
    case seratoFaithful = 0
    case highContrast = 1
    case monochrome = 2

    public var id: UInt32 { rawValue }
    public var displayName: String {
        switch self {
        case .seratoFaithful: return "Serato-faithful"
        case .highContrast:   return "High contrast"
        case .monochrome:     return "Monochrome"
        }
    }
}

/// 12-byte mirror of `PeakChunk` for memory-layout assertions.
/// Generated UniFFI bindings return chunks as `Data`; we treat that
/// Data as `[PeakChunk]` via `withUnsafeBytes(_:)`.
private struct PeakChunkLayout {
    var minSample: Float
    var maxSample: Float
    var rms: Float
}

/// 32-byte mirror of `BandPeakChunk` for memory-layout assertions.
/// Matches `#[repr(C)] pub struct BandPeakChunk { pub rms_per_band:
/// [f32; 8] }` — packed array, no padding.
private struct BandPeakChunkLayout {
    var b0: Float; var b1: Float; var b2: Float; var b3: Float
    var b4: Float; var b5: Float; var b6: Float; var b7: Float
}

@MainActor
final class WaveformRenderer: NSObject {

    // MARK: Configuration

    /// Power-of-two number of broadband chunks the GPU ring buffer
    /// can hold. 2^20 = 1 048 576 chunks ≈ 1 400 s of audio at 48 kHz /
    /// 64-sample chunks (~23 min). Sized so the entire offline-decoded
    /// peak set of any realistically-long DJ track (M10.5 File mode)
    /// fits in the ring without head-wrap collisions during a seek
    /// back to the start of a long track. The Thru-mode case never
    /// needed more than ~175 s, but oversizing here costs only ~12 MB
    /// per deck of unified memory on Apple Silicon. Power-of-two so
    /// the modulo math compiles to a bit-mask.
    static let chunkCapacity: Int = 1_048_576

    /// Power-of-two number of band chunks the GPU ring buffer can
    /// hold. Band chunks occur once per `BAND_SAMPLES_PER_CHUNK`
    /// audio samples (= 512), broadband chunks once per 64 — so
    /// per second of audio we get 8× fewer band chunks. 2^17 =
    /// 131 072 band chunks → ~1 400 s at 48 kHz, matching the
    /// broadband ring's coverage. Power-of-two so the shader's
    /// `(idx % bandCapacity)` compiles to a mask.
    static let bandChunkCapacity: Int = 131_072

    /// Maximum number of frames the CPU is allowed to queue ahead
    /// of the GPU. The standard "three frames in flight" Metal
    /// pattern; matches `MTKView`'s default `maxBufferCountInFlight`.
    /// Equal to the uniform buffer count below.
    static let maxFramesInFlight: Int = 3

    /// Amplitude scale for the waveform (NDC). 0.95 keeps the bars
    /// inside the viewport. M10.4 rotated the waveform: this scale
    /// now applies to the horizontal axis (amplitude), not vertical.
    private static let yScale: Float = 0.95

    /// Fraction of the deck column's height reserved for the *past*
    /// region (above the playhead). The playhead lives at 25 % from
    /// the top per PRD §9.1, so 25 % of the height holds past peaks.
    /// The remaining 75 % below the playhead is the "future" region,
    /// reserved for M10.5's pre-decoded File-mode peaks; empty in
    /// Thru mode.
    static let pastRegionFraction: Double = 0.25

    /// Approximate chunks per pixel along the *time axis*. The
    /// time axis is now vertical (M10.4); the value is unchanged
    /// in absolute units (~4 ms per pixel at 48 kHz / 64-sample
    /// chunks) so a ~280-px past-region strip shows ~1.1 s of
    /// played history — comfortable for the audience-facing eye
    /// (a kick passes through the playhead and stays visible for
    /// roughly one bar at 120 BPM).
    private static let chunksPerPixel: Double = 4.0

    // MARK: Dependencies

    let device: MTLDevice
    private let commandQueue: MTLCommandQueue
    private let pipelineState: MTLRenderPipelineState

    /// Bounded queue depth via semaphore. Prevents the CPU from
    /// writing into a uniform buffer the GPU is still reading.
    private let inflightSemaphore = DispatchSemaphore(value: maxFramesInFlight)

    /// Triple-buffered uniforms — one slot per inflight frame.
    private var uniformBuffers: [MTLBuffer] = []
    private var uniformIndex: Int = 0

    /// Append-only ring buffer of `PeakChunk`s. Shared storage so
    /// we can memcpy directly from the FFI `Data` blob without an
    /// intermediate blit. Stable storage class on Apple Silicon
    /// makes the GPU read from the same physical pages — zero-copy
    /// after the initial CPU write.
    private let chunksBuffer: MTLBuffer

    /// Append-only ring buffer of `BandPeakChunk`s (M10.1).
    /// Parallel to `chunksBuffer`; the vertex shader looks up the
    /// containing band chunk for each broadband instance.
    private let bandChunksBuffer: MTLBuffer

    // MARK: Engine binding

    /// Holds the engine reference and the deck index this renderer
    /// is observing. The renderer never mutates the engine; it only
    /// polls `peaksLen` / `peaksExtend`.
    private let engine: DubEngine
    private let deckIdx: UInt64

    /// How many broadband chunks have been written into the ring so
    /// far. Modulo `chunkCapacity` gives the ring offset.
    private(set) var totalChunksAppended: UInt64 = 0

    /// How many band chunks have been written into the ring so far.
    /// Modulo `bandChunkCapacity` gives the ring offset.
    private(set) var totalBandChunksAppended: UInt64 = 0

    /// Cached `peaks_len()` from the previous poll; renderer pulls
    /// only the *new* chunks each frame.
    private var lastSeenPeaksLen: UInt64 = 0

    /// Cached `band_peaks_len()` from the previous poll. Tracked
    /// independently of `lastSeenPeaksLen` because the two streams
    /// progress at different cadences (one band chunk per 8
    /// broadband chunks).
    private var lastSeenBandPeaksLen: UInt64 = 0

    /// Cached `peaks_generation()` from the previous poll. When the
    /// engine swaps a deck's `PeakSource` (Thru → File on track
    /// load, File → File on reload) this counter bumps; the
    /// renderer responds by wiping its ring buffer + cadence cache
    /// before re-ingesting from chunk 0. Without this the
    /// length-monotonicity heuristic in `ingestNewChunks` silently
    /// no-ops the post-swap path (the new source's chunk count is
    /// smaller than what we last observed under the previous
    /// source) and the renderer keeps drawing stale data forever.
    private var lastSeenPeaksGeneration: UInt64 = 0

    /// Cached `samples_per_peak_chunk` and `samples_per_band_chunk`
    /// values, read once on engine startup. Pinned at the M9
    /// defaults (64 / 512) for now but read through the FFI so
    /// future tuning doesn't require a renderer change.
    ///
    /// Initialised lazily on the first non-empty poll because the
    /// engine hasn't been started yet when `init` runs.
    private var samplesPerPeakChunk: UInt32 = 64
    private var samplesPerBandChunk: UInt32 = 512

    /// Active palette. Updated by `MainView` from the SwiftUI
    /// settings sub-view; the renderer reads it on every `draw`.
    var palette: WaveformPalette = .seratoFaithful

    // MARK: Init

    /// Construct a renderer for the given Metal device. Throws if
    /// the pipeline can't be built (almost always a developer error
    /// — missing `.metal` source, bad function names, malformed
    /// vertex descriptor).
    init(device: MTLDevice, engine: DubEngine, deckIdx: UInt64 = 0) throws {
        self.device = device
        self.engine = engine
        self.deckIdx = deckIdx

        guard let queue = device.makeCommandQueue() else {
            throw NSError(
                domain: "WaveformRenderer", code: 1,
                userInfo: [NSLocalizedDescriptionKey: "Metal command queue allocation failed"])
        }
        self.commandQueue = queue
        self.commandQueue.label = "dub.waveform.cmdqueue"

        // Library lookup: when the .metal file ships in the app
        // bundle, `makeDefaultLibrary` is sufficient. Errors here
        // are typically "missing .metal in the target" — surface
        // them with a clear message.
        let library: MTLLibrary
        do {
            library = try device.makeDefaultLibrary(bundle: Bundle.main)
        } catch {
            throw NSError(
                domain: "WaveformRenderer", code: 2,
                userInfo: [
                    NSLocalizedDescriptionKey: "Default Metal library load failed: \(error)"
                ])
        }
        guard let vertexFn = library.makeFunction(name: "waveformVertex"),
              let fragmentFn = library.makeFunction(name: "waveformFragment")
        else {
            throw NSError(
                domain: "WaveformRenderer", code: 3,
                userInfo: [
                    NSLocalizedDescriptionKey:
                        "Metal functions waveformVertex / waveformFragment not found in default library"
                ])
        }

        let descriptor = MTLRenderPipelineDescriptor()
        descriptor.label = "dub.waveform.pipeline"
        descriptor.vertexFunction = vertexFn
        descriptor.fragmentFunction = fragmentFn
        descriptor.colorAttachments[0].pixelFormat = .bgra8Unorm
        // M10-B is monochrome — no alpha blending. M10.1 may
        // enable alpha blending if it stacks transparent colour
        // passes for onset glow.
        descriptor.colorAttachments[0].isBlendingEnabled = false

        self.pipelineState = try device.makeRenderPipelineState(descriptor: descriptor)

        // Allocate the broadband chunks ring. Storage mode `.shared`
        // works on both Apple Silicon (unified memory, zero-copy)
        // and Intel macs (the small bandwidth hit is negligible at
        // ~1.5 MB per deck for broadband + ~0.5 MB per deck for
        // bands).
        let chunkBytes = WaveformRenderer.chunkCapacity * MemoryLayout<PeakChunkLayout>.stride
        guard let chunks = device.makeBuffer(length: chunkBytes, options: .storageModeShared) else {
            throw NSError(
                domain: "WaveformRenderer", code: 4,
                userInfo: [NSLocalizedDescriptionKey: "Chunks MTLBuffer allocation failed"])
        }
        chunks.label = "dub.waveform.chunks"
        // Zero out so the first frame after start_thru renders a
        // flat line (rather than a wall of uninitialised garbage).
        chunks.contents().initializeMemory(as: UInt8.self, repeating: 0, count: chunkBytes)
        self.chunksBuffer = chunks

        // Allocate the band chunks ring (M10.1).
        let bandChunkBytes =
            WaveformRenderer.bandChunkCapacity * MemoryLayout<BandPeakChunkLayout>.stride
        guard
            let bandChunks = device.makeBuffer(
                length: bandChunkBytes, options: .storageModeShared)
        else {
            throw NSError(
                domain: "WaveformRenderer", code: 4,
                userInfo: [
                    NSLocalizedDescriptionKey: "Band chunks MTLBuffer allocation failed"
                ])
        }
        bandChunks.label = "dub.waveform.bandChunks"
        bandChunks.contents().initializeMemory(
            as: UInt8.self, repeating: 0, count: bandChunkBytes)
        self.bandChunksBuffer = bandChunks

        // Allocate uniform buffers (one per in-flight frame).
        let uniformBytes = MemoryLayout<WaveformUniforms>.stride
        var uniforms: [MTLBuffer] = []
        for idx in 0..<WaveformRenderer.maxFramesInFlight {
            guard let buf = device.makeBuffer(length: uniformBytes, options: .storageModeShared)
            else {
                throw NSError(
                    domain: "WaveformRenderer", code: 5,
                    userInfo: [NSLocalizedDescriptionKey: "Uniform MTLBuffer allocation failed"])
            }
            buf.label = "dub.waveform.uniforms[\(idx)]"
            uniforms.append(buf)
        }
        self.uniformBuffers = uniforms

        super.init()
    }

    /// Drop any cached state. Called by `WaveformView` when the engine
    /// is stopped or the deck rebinds. Idempotent.
    func reset() {
        totalChunksAppended = 0
        totalBandChunksAppended = 0
        lastSeenPeaksLen = 0
        lastSeenBandPeaksLen = 0
        lastSeenPeaksGeneration = 0
        // Force a re-snapshot of the per-source chunk-cadence on the
        // next ingest so a Thru → File swap (different sample
        // rates → different `samples_per_*_chunk`) doesn't keep
        // computing time math against the previous source's cadence.
        samplesPerPeakChunk = 64
        samplesPerBandChunk = 512
        // Zeroing both buffers means the next frame after a restart
        // shows silence at the right edge of the viewport instead
        // of stale audio.
        let chunkBytes = WaveformRenderer.chunkCapacity * MemoryLayout<PeakChunkLayout>.stride
        chunksBuffer.contents().initializeMemory(as: UInt8.self, repeating: 0, count: chunkBytes)
        let bandChunkBytes =
            WaveformRenderer.bandChunkCapacity * MemoryLayout<BandPeakChunkLayout>.stride
        bandChunksBuffer.contents().initializeMemory(
            as: UInt8.self, repeating: 0, count: bandChunkBytes)
    }

    // MARK: MTKViewDelegate-style entry points

    /// Called from `WaveformView.Coordinator.mtkView(_:drawableSizeWillChange:)`.
    /// The pixel-density change drives `chunksVisible`, so we don't
    /// cache anything here — the next `draw(in:)` recomputes
    /// `chunksVisible` from `view.drawableSize`.
    func drawableSizeWillChange(_ size: CGSize) {
        _ = size
    }

    /// Per-frame work. Polls the engine for new chunks, uploads
    /// them into the ring, and records a single draw call.
    func draw(in view: MTKView) {
        // Bound CPU queue depth. `.now()` (no timeout) blocks
        // indefinitely if the GPU is wedged, which matches Apple's
        // recommended pattern — a hung GPU is a fatal-environment
        // condition we wouldn't recover from anyway.
        inflightSemaphore.wait()

        let releaseSemaphore: () -> Void = { [weak self] in
            self?.inflightSemaphore.signal()
        }

        // 0. Detect a `PeakSource` swap on this deck. When the
        //    engine swaps Thru → File (drag-and-drop load) or
        //    File → File (reload) the per-deck generation counter
        //    bumps; we wipe the ring and re-ingest the new source
        //    from chunk 0. Doing this BEFORE the ingest pull is
        //    critical: the new source's chunk count is typically
        //    smaller than what we last observed from Thru, so the
        //    length-monotonicity check in `ingestNewChunks` would
        //    otherwise silently no-op and the renderer would keep
        //    drawing stale Thru capture forever.
        let currentGeneration = engine.peaksGeneration(deckIdx: deckIdx)
        if currentGeneration != lastSeenPeaksGeneration {
            reset()
            lastSeenPeaksGeneration = currentGeneration
        }

        // 1. Pull any newly-available chunks from the engine
        //    (broadband + bands).
        ingestNewChunks()
        ingestNewBandChunks()

        // 2. Compute the visible window.
        //
        // M10.5b piecewise vertical layout: the time axis is the
        // deck pane's height. The playhead lives at 25 % from the
        // top (PRD §9.1) and we render BOTH a past region above it
        // (top 25 %) and a future region below it (bottom 75 %).
        //
        //   Past pixels   ≈ height * 0.25  → chunksAbovePlayhead
        //                                    instances rendered with
        //                                    NDC y ∈ [+0.5, +1.0]
        //   Future pixels ≈ height * 0.75  → chunksBelowPlayhead
        //                                    instances rendered with
        //                                    NDC y ∈ [-1.0, +0.5]
        //
        // Thru mode (no future data): `chunksBelowPlayhead = 0`, so
        // only the past branch fires — identical to M10.4 behaviour.
        let drawableSize = view.drawableSize
        let pixelHeight = max(1, Int(drawableSize.height))
        let pastPixels =
            max(1, Int((Double(pixelHeight) * WaveformRenderer.pastRegionFraction).rounded()))
        let futurePixels =
            max(0, Int((Double(pixelHeight)
                * (1.0 - WaveformRenderer.pastRegionFraction)).rounded()))
        let chunksAbovePixels =
            Int((Double(pastPixels) * WaveformRenderer.chunksPerPixel).rounded())
        let chunksBelowPixels =
            Int((Double(futurePixels) * WaveformRenderer.chunksPerPixel).rounded())

        // Figure out the playhead chunk + how many chunks we have
        // past it. The renderer asks the engine for the position; a
        // `has_track` deck (File mode) drives the playhead from
        // `elapsed_secs`. Otherwise (Thru mode, empty deck) the
        // playhead = newest chunk just appended.
        let pos = engine.position(deckIdx: deckIdx)
        let peaksLenGlobal = totalChunksAppended
        let hasFuture = pos.hasTrack
        let playheadChunk: UInt64
        if hasFuture {
            // File mode. Map elapsed seconds → chunk index using the
            // captured cadence.
            let sr = Double(engine.sampleRate())
            let spc = Double(samplesPerPeakChunk)
            if sr > 0, spc > 0 {
                let elapsedSamples = pos.elapsedSecs * sr
                let chunkF = (elapsedSamples / spc).rounded(.down)
                let chunkClamped = max(0.0, min(chunkF, Double(peaksLenGlobal &- 1)))
                playheadChunk = UInt64(chunkClamped)
            } else {
                playheadChunk = peaksLenGlobal == 0 ? 0 : peaksLenGlobal &- 1
            }
        } else {
            // Thru mode (or empty deck). The newest chunk is the
            // playhead. peaksLenGlobal == 0 ⇒ nothing yet.
            playheadChunk = peaksLenGlobal == 0 ? 0 : peaksLenGlobal &- 1
        }

        // How many ring slots are valid behind / ahead of the
        // playhead? Past region includes the playhead chunk itself
        // (i.e. "the chunk just played" is the bottom of the past
        // region, sitting just above the y=+0.5 NDC playhead line —
        // this matches M10.4 behaviour). Future region starts at
        // `playheadChunk + 1`.
        let chunksAvailableBehind: Int
        let chunksAvailableAhead: Int
        if peaksLenGlobal == 0 {
            chunksAvailableBehind = 0
            chunksAvailableAhead = 0
        } else {
            chunksAvailableBehind = min(Int(playheadChunk) + 1, WaveformRenderer.chunkCapacity)
            if hasFuture {
                chunksAvailableAhead = min(
                    Int(peaksLenGlobal &- 1 &- playheadChunk),
                    WaveformRenderer.chunkCapacity)
            } else {
                chunksAvailableAhead = 0
            }
        }

        let chunksAbove = max(0, min(chunksAbovePixels, chunksAvailableBehind))
        let chunksBelow = max(0, min(chunksBelowPixels, chunksAvailableAhead))
        let chunksVisible = chunksAbove + chunksBelow

        // Bail out early if there's nothing visible yet (just
        // present a clear background).
        guard peaksLenGlobal > 0,
              chunksVisible > 0,
              let drawable = view.currentDrawable,
              let passDescriptor = view.currentRenderPassDescriptor
        else {
            releaseSemaphore()
            return
        }

        // First instance (iid = 0) is the oldest visible past chunk.
        // The past region renders chunks [playheadChunk - chunksAbove
        // + 1, playheadChunk]; the future region renders chunks
        // [playheadChunk + 1, playheadChunk + chunksBelow]. Combined
        // ring window starts at `playheadChunk + 1 - chunksAbove`.
        // When chunksAbove == 0 (future-only), this lands one past
        // the playhead — also correct.
        let oldestGlobal = (playheadChunk &+ 1) &- UInt64(chunksAbove)
        let oldestRingOffset = Int(oldestGlobal % UInt64(WaveformRenderer.chunkCapacity))

        // Band ring offset matching the *audio-time* start of the
        // visible window. Band chunks cover `samplesPerBandChunk`
        // audio samples each; oldest visible audio sample =
        // `oldestGlobal * samplesPerPeakChunk`. Modulo bandCapacity.
        let bandPerSample = max(UInt64(samplesPerBandChunk), 1)
        let oldestSample = oldestGlobal &* UInt64(samplesPerPeakChunk)
        let oldestBandGlobal = oldestSample / bandPerSample
        let oldestBandRingOffset =
            Int(oldestBandGlobal % UInt64(WaveformRenderer.bandChunkCapacity))

        // 3. Fill the current uniform buffer.
        let uniforms = WaveformUniforms(
            chunkOffset: UInt32(oldestRingOffset),
            chunksVisible: UInt32(chunksVisible),
            chunksAbovePlayhead: UInt32(chunksAbove),
            yScale: WaveformRenderer.yScale,
            samplesPerPeakChunk: samplesPerPeakChunk,
            bandChunkOffset: UInt32(oldestBandRingOffset),
            samplesPerBandChunk: samplesPerBandChunk,
            bandCapacity: UInt32(WaveformRenderer.bandChunkCapacity),
            palette: palette.rawValue)
        let uniformBuffer = uniformBuffers[uniformIndex]
        uniformBuffer.contents().withMemoryRebound(to: WaveformUniforms.self, capacity: 1) { ptr in
            ptr.pointee = uniforms
        }

        // 4. Record the render pass.
        guard let commandBuffer = commandQueue.makeCommandBuffer() else {
            releaseSemaphore()
            return
        }
        commandBuffer.label = "dub.waveform.commandBuffer"
        commandBuffer.addCompletedHandler { _ in releaseSemaphore() }

        guard let encoder = commandBuffer.makeRenderCommandEncoder(descriptor: passDescriptor)
        else {
            // Encoder may be nil if MTKView is mid-resize.
            releaseSemaphore()
            return
        }
        encoder.label = "dub.waveform.encoder"
        encoder.setRenderPipelineState(pipelineState)
        encoder.setVertexBuffer(uniformBuffer, offset: 0, index: 0)
        encoder.setVertexBuffer(chunksBuffer, offset: 0, index: 1)
        encoder.setVertexBuffer(bandChunksBuffer, offset: 0, index: 2)

        // 4 verts per instance (triangle strip), one instance per
        // chunk. `chunksVisible` instances × 4 verts → 4 ×
        // chunksVisible vertex invocations.
        encoder.drawPrimitives(
            type: .triangleStrip, vertexStart: 0, vertexCount: 4, instanceCount: chunksVisible)
        encoder.endEncoding()

        commandBuffer.present(drawable)
        commandBuffer.commit()

        // 5. Rotate uniform buffer slot for next frame.
        uniformIndex = (uniformIndex + 1) % WaveformRenderer.maxFramesInFlight
    }

    // MARK: Ingestion

    /// Pull any newly-available `PeakChunk`s from the engine and
    /// memcpy them into the GPU ring buffer. Bounded work per
    /// frame: at most `chunkCapacity` new chunks fit in the ring;
    /// older ones are wrapped over (the same chunk index modulo
    /// `chunkCapacity` is reused).
    private func ingestNewChunks() {
        let currentLen = engine.peaksLen(deckIdx: deckIdx)
        if currentLen <= lastSeenPeaksLen {
            return
        }
        // First non-empty poll: snapshot the engine's chunk
        // cadences so the band-lookup math in the shader matches
        // reality. The engine reports durations as
        // `samplesPerChunk / sampleRate`; we multiply by the
        // sample rate to recover `samplesPerChunk` as a `u32`.
        // Both cadences are pinned by the M9 / M9.5b defaults
        // today, but reading them from the FFI keeps the renderer
        // honest if those constants ever change.
        if lastSeenPeaksLen == 0 {
            captureChunkCadences()
        }
        // `peaksExtend` returns chunks at indices [startIdx,
        // engine.peaksLen). Cap fetched chunks at `chunkCapacity`
        // so a long stall (e.g. CPU-pinned UI thread) doesn't
        // memcpy gigabytes when we resume.
        let availableNew = currentLen &- lastSeenPeaksLen
        let cappedNew = min(availableNew, UInt64(WaveformRenderer.chunkCapacity))
        let startIdx = currentLen &- cappedNew
        let data = engine.peaksExtend(deckIdx: deckIdx, startIdx: startIdx)
        if data.isEmpty {
            // Probably hit a stop-thru race; re-sync on next poll.
            lastSeenPeaksLen = currentLen
            return
        }

        let chunkStride = MemoryLayout<PeakChunkLayout>.stride
        let newChunkCount = data.count / chunkStride

        // Defensive: if the Rust side ever changes the stride
        // without bumping FFI_VERSION, we'd otherwise scribble
        // misaligned bytes into the ring. Drop the update
        // silently — the user sees a stale waveform, not a crash.
        guard newChunkCount > 0, data.count % chunkStride == 0 else {
            lastSeenPeaksLen = currentLen
            return
        }

        let ringBytes = WaveformRenderer.chunkCapacity * chunkStride
        let dstBase = chunksBuffer.contents()

        data.withUnsafeBytes { (rawSrc: UnsafeRawBufferPointer) in
            guard let srcBase = rawSrc.baseAddress else { return }

            // Write into the ring starting at the slot for `startIdx`
            // (= currentLen - newChunkCount globally → modulo
            // chunkCapacity). Wraps once at most because we capped
            // `cappedNew` at chunkCapacity.
            let firstSlot = Int(startIdx % UInt64(WaveformRenderer.chunkCapacity))
            let bytesToWrite = newChunkCount * chunkStride
            let firstByteOffset = firstSlot * chunkStride

            if firstByteOffset + bytesToWrite <= ringBytes {
                // Contiguous write — fast path.
                memcpy(dstBase.advanced(by: firstByteOffset), srcBase, bytesToWrite)
            } else {
                // Wrap: write tail of ring + head of ring.
                let bytesBeforeWrap = ringBytes - firstByteOffset
                memcpy(dstBase.advanced(by: firstByteOffset), srcBase, bytesBeforeWrap)
                memcpy(
                    dstBase,
                    srcBase.advanced(by: bytesBeforeWrap),
                    bytesToWrite - bytesBeforeWrap)
            }
        }

        // Update bookkeeping. `totalChunksAppended` advances by the
        // engine-reported total, not by `newChunkCount`, so the
        // global index stays in sync with the Rust side even if
        // `cappedNew` truncated this frame's catch-up.
        totalChunksAppended = currentLen
        lastSeenPeaksLen = currentLen
    }

    /// Pull any newly-available `BandPeakChunk`s and memcpy them
    /// into the band ring. Mirror image of [`ingestNewChunks`] for
    /// the M9.5b parallel band stream.
    ///
    /// Engineering note: returns early if the engine reports
    /// `bandPeaksChunkDurationSecs == 0`, which means band capture
    /// was disabled for this `PeakStream`. In that case the renderer
    /// still shows a (monochrome-ish) bar — the vertex shader will
    /// see all-zero band values and the fragment will drop to its
    /// neutral grey fallback.
    private func ingestNewBandChunks() {
        let currentLen = engine.bandPeaksLen(deckIdx: deckIdx)
        if currentLen <= lastSeenBandPeaksLen {
            return
        }
        let availableNew = currentLen &- lastSeenBandPeaksLen
        let cappedNew = min(availableNew, UInt64(WaveformRenderer.bandChunkCapacity))
        let startIdx = currentLen &- cappedNew
        let data = engine.bandPeaksExtend(deckIdx: deckIdx, startIdx: startIdx)
        if data.isEmpty {
            lastSeenBandPeaksLen = currentLen
            return
        }

        let chunkStride = MemoryLayout<BandPeakChunkLayout>.stride
        let newChunkCount = data.count / chunkStride

        guard newChunkCount > 0, data.count % chunkStride == 0 else {
            lastSeenBandPeaksLen = currentLen
            return
        }

        let ringBytes = WaveformRenderer.bandChunkCapacity * chunkStride
        let dstBase = bandChunksBuffer.contents()

        data.withUnsafeBytes { (rawSrc: UnsafeRawBufferPointer) in
            guard let srcBase = rawSrc.baseAddress else { return }
            let firstSlot = Int(startIdx % UInt64(WaveformRenderer.bandChunkCapacity))
            let bytesToWrite = newChunkCount * chunkStride
            let firstByteOffset = firstSlot * chunkStride

            if firstByteOffset + bytesToWrite <= ringBytes {
                memcpy(dstBase.advanced(by: firstByteOffset), srcBase, bytesToWrite)
            } else {
                let bytesBeforeWrap = ringBytes - firstByteOffset
                memcpy(dstBase.advanced(by: firstByteOffset), srcBase, bytesBeforeWrap)
                memcpy(
                    dstBase,
                    srcBase.advanced(by: bytesBeforeWrap),
                    bytesToWrite - bytesBeforeWrap)
            }
        }

        totalBandChunksAppended = currentLen
        lastSeenBandPeaksLen = currentLen
    }

    /// Snapshot the engine's reported broadband + band chunk
    /// cadences (samples-per-chunk) the first time a poll returns
    /// non-zero data. Cached so subsequent draws don't pay the
    /// FFI cost. Combines `sample_rate` (Hz) with the per-stream
    /// `chunk_duration_secs` to recover the exact integer
    /// `samples_per_chunk` cadence the shader needs to map
    /// broadband instance IDs to band ring offsets.
    ///
    /// Falls back to the M9 / M9.5b defaults (64 / 512) if the
    /// engine returns 0 for either accessor (e.g. band capture
    /// disabled, engine stopped between poll and follow-up).
    private func captureChunkCadences() {
        let sr = engine.sampleRate()
        if sr == 0 {
            return
        }
        let srD = Double(sr)
        let peakDur = engine.peaksChunkDurationSecs(deckIdx: deckIdx)
        let bandDur = engine.bandPeaksChunkDurationSecs(deckIdx: deckIdx)
        if peakDur > 0 {
            let samples = Int((peakDur * srD).rounded())
            if samples > 0 {
                samplesPerPeakChunk = UInt32(samples)
            }
        }
        if bandDur > 0 {
            let samples = Int((bandDur * srD).rounded())
            if samples > 0 {
                samplesPerBandChunk = UInt32(samples)
            }
        }
    }
}
