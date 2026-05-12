//
//  MainWindowController.swift
//  Dub
//
//  Top-level window for the Dub shell. Hosts a SwiftUI `MainView`
//  (waveform + device picker) inside an `NSHostingController`. The
//  M0.5 smoke-screen text now lives as a debug overlay inside
//  `MainView`.
//

import AppKit
import SwiftUI

final class MainWindowController: NSWindowController {
    convenience init() {
        // Default content size for first launch. AppKit will *resize* the
        // window to whatever the hosting controller reports as its
        // `preferredContentSize` once we assign `contentViewController`
        // — without an explicit preferred size, SwiftUI's intrinsic
        // size for `MainView` collapses to the toolbar's footprint
        // (~514×87) because the embedded `MTKView` has no intrinsic
        // size. Setting `preferredContentSize` *before* attaching the
        // controller pins the size correctly; re-asserting via
        // `setContentSize` after attachment is belt-and-braces for the
        // edge case where the preferred size hasn't propagated yet.
        // M10.3 sizes the default window to the PRD §9.2 reference
        // rectangle (the same canvas the Figma exploration used) so
        // the layout out of the box matches the documented layout.
        // `minSize` below stays at 720×480 — the layout remains
        // sensible at much smaller sizes; this is just the natural
        // first-launch dimension.
        let defaultSize = NSSize(width: 1440, height: 900)

        let hostingController = NSHostingController(rootView: MainView())
        hostingController.preferredContentSize = defaultSize

        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: defaultSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Dub"
        window.contentViewController = hostingController
        window.setContentSize(defaultSize)
        window.minSize = NSSize(width: 720, height: 480)
        window.center()

        self.init(window: window)
    }
}
