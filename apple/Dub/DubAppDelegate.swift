//
//  DubAppDelegate.swift
//  Dub
//
//  AppKit lifecycle for the Dub macOS shell. Keeps the AppKit entry point
//  lean — actual content lives in `MainWindowController`, which hosts a
//  SwiftUI view via `NSHostingController`. This hybrid pattern is
//  deliberate: AppKit owns the lifecycle (it has the lowest-overhead
//  hooks we'll need for the audio HUD in M10), SwiftUI owns the
//  non-realtime sub-views (settings, library, etc.).
//
//  The explicit `static func main()` exists because the default
//  `NSApplicationDelegate.main()` in the current macOS Swift overlay
//  calls `NSApplicationMain` *without* installing an instance as
//  `NSApp.delegate`. Without that wiring the app launches into an
//  event loop with no delegate, `applicationDidFinishLaunching` is
//  never invoked, no window is created, and the user sees only a
//  menu bar. (UIKit's overlay does the right thing; AppKit's does
//  not unless you also load a `MainMenu.xib` that sets the delegate
//  on nib-load — we don't ship a nib by design.) Holding the instance
//  in a static guarantees it outlives `NSApp.delegate`'s weak
//  reference for the program's lifetime.
//

import AppKit

@main
final class DubAppDelegate: NSObject, NSApplicationDelegate {
    private static let sharedDelegate = DubAppDelegate()

    private var mainWindowController: MainWindowController?

    static func main() {
        let app = NSApplication.shared
        app.delegate = sharedDelegate
        _ = NSApplicationMain(CommandLine.argc, CommandLine.unsafeArgv)
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)

        let controller = MainWindowController()
        controller.showWindow(self)
        self.mainWindowController = controller

        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}
