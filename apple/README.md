# Dub macOS app — `apple/`

This directory hosts the AppKit/SwiftUI shell that consumes the Rust core
through UniFFI.

## Status: M10.2 — polish (first wave shipped)

The app opens a 1280×800 window with a real input-device picker, two
channel fields (`chA` defaults to `1,2`; `chB` empty = single-deck,
or e.g. `5,6` to wire a second deck), a palette menu
(Serato-faithful / high-contrast / monochrome), and a Metal-rendered
multi-colour waveform that scrolls in real time at 60 fps once you
hit Start. The shader mixes the 8-band perceptual loudness data
(M9.5b) into RGB — bass → red, mids → green, highs → blue — with
broadband-RMS luminance preserving the amplitude shape. Clipped
bars paint solid red; silent stretches drop to a thin neutral
hairline. When deck B is configured, the waveform area splits
vertically via `VSplitView` and renders both decks
independently. The M0.5 `"Dub engine OK · v<version>"` text now
lives as a thin debug overlay at the bottom of the waveform
(also includes the current sample rate).

## One-shot bootstrap

```bash
brew install xcodegen                  # one-time
./scripts/bootstrap.sh                 # xcframework + Swift bindings + Xcode project
open apple/Dub.xcodeproj
# ⌘R, pick an input, set channels (e.g. 1,2 or 3,4 for SL3), Start.
```

Re-run `./scripts/bootstrap.sh` whenever:

- `apple/project.yml` changes — regenerates `Dub.xcodeproj`.
- `crates/dub-ffi/src/lib.rs` changes — rebuilds the xcframework and
  regenerates the Swift bindings.
- `apple/Dub/Waveform/Shaders.metal` changes — Xcode rebuilds the
  `default.metallib` automatically on the next build.

## Layout

```
apple/
├── project.yml                    XcodeGen manifest (source of truth)
├── Dub.xcodeproj/                 Generated (gitignored)
├── DubCore.xcframework/           Generated (gitignored) — universal Rust static lib
├── Dub/
│   ├── DubAppDelegate.swift       AppKit @main lifecycle
│   ├── MainWindowController.swift NSWindow holding an NSHostingController(MainView)
│   ├── MainView.swift             SwiftUI: device picker + chA/chB channels + palette menu + Start/Stop + waveform(s)
│   ├── Waveform/
│   │   ├── Shaders.metal          Vertex (instanced quads) + fragment shaders
│   │   ├── WaveformRenderer.swift @MainActor Metal renderer (chunks ring, triple-buffered uniforms)
│   │   └── WaveformView.swift     NSViewRepresentable wrapping MTKView
│   ├── Info.plist                 Placeholder — keys overridden by XcodeGen
│   └── Dub.entitlements           Sandbox off (local-only signing)
└── DubShared/
    ├── Package.swift              Swift Package wrapping DubCore.xcframework
    └── Sources/DubCore/
        ├── Placeholder.swift
        └── Generated/             UniFFI Swift bindings (gitignored)
```

## Why a hybrid AppKit + SwiftUI app?

- **AppKit owns the lifecycle.** The M10 waveform needs `MTKView`
  through `NSViewRepresentable`, plus future `NSEvent` hooks that
  SwiftUI either doesn't expose or re-exposes through awkward bridges.
  AppKit gives us the lowest-overhead path.
- **SwiftUI owns non-realtime sub-views.** `MainView`, the upcoming
  M10.2 palette picker, and future library / settings sheets are pure
  forms; SwiftUI's declarative model is faster to iterate on than
  AppKit's view-by-view layout.
- **The waveform itself is `MTKView` (Metal) wrapped in
  `NSViewRepresentable`.** The renderer polls `DubEngine.peaksLen` +
  `peaksExtend` each frame; no callback path from the audio thread
  ever reaches Swift.

## Signing

M0.5 ships with **"Sign to Run Locally"** only. No Apple Developer
account, no notarisation. Distribution signing lands as its own
milestone after M10.2.

## See also

- `docs/PRD.md` §10.1 — Workspace layout
- `docs/PRD.md` §10.3 — Apple frontend stack
- `.cursor/rules/swift.mdc` — Swift conventions
- `.cursor/rules/ffi.mdc` — Rust ↔ Swift contract
