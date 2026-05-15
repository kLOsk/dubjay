//
//  FileBrowserView.swift
//  Dub
//
//  M10.5b slim filesystem browser. Sits in the LIBRARY region of the
//  Performance View and replaces the M10.3 `LibraryPlaceholder` for
//  M10.5b dev-loop testing. **Intentionally below M11** — no library
//  database, no crate concept, no Serato / Traktor import.
//
//  PRD §9.7 (TBD) + M10.5b spec:
//      • Folder picker at the top (NSOpenPanel — directory only).
//      • Listing pane: subfolders first, then audio files.
//      • Single-click on **any row** (file *or* folder) selects it —
//        the row highlights and `model.browserSelection` updates.
//        Space loads the selected file into the non-master stopped
//        deck (PRD §5.5); Space on a folder is a no-op with a
//        polite error (folders aren't audio).
//      • **Double-click on a folder** descends into it. Files are
//        intentionally non-double-clickable to load (per the
//        user's M10.5b decision — files load via Space or drag).
//      • Drag-and-drop a file from this view onto a deck pane also
//        works. The drag uses the legacy AppKit `.onDrag {
//        NSItemProvider }` path rather than SwiftUI's modern
//        `.draggable(_:preview:)` API: the latter renders the
//        preview closure into the row's coordinate space *first*
//        and then animates it toward the cursor at drag start,
//        producing a visible "fly-in" from where the row sits.
//        AppKit's drag path takes a snapshot of the source view
//        and anchors it under the cursor at the mouse-down point,
//        which is the OS-native feel we want. The drop side
//        (`.dropDestination(for: URL.self)`) reads `public.file-
//        url` from the pasteboard either way.
//

import AppKit
import SwiftUI
import UniformTypeIdentifiers

import DubCore

/// Audio file extensions the browser shows. Conservative set —
/// formats `dub-io` is known to decode through `symphonia`. Future
/// formats (Opus, etc.) get added here as they pass smoke-load.
private let kAudioExtensions: Set<String> = [
    "mp3", "m4a", "aac", "wav", "wave", "aif", "aiff", "flac", "ogg",
]

/// One row in the browser. Either a subdirectory or an audio file.
struct BrowserEntry: Identifiable, Hashable {
    let url: URL
    let isDirectory: Bool

    var id: URL { url }

    var displayName: String {
        url.lastPathComponent
    }

    var ext: String {
        url.pathExtension.uppercased()
    }
}

/// Per-folder cache of container-tag metadata (M10.5r). Reading
/// ID3 / Vorbis / MP4-atom tags goes through `readTrackMetadata`
/// which opens the file, probes the metadata block, closes — ~1 ms
/// on a warm filesystem but ~1 000 files × 1 ms = a visible second
/// on first folder load, so we move the work off the main actor
/// and stream results in as they arrive.
///
/// Lives as an `ObservableObject` so the view can react to per-row
/// completions without forcing a full re-render of the listing.
@MainActor
final class BrowserMetadataCache: ObservableObject {
    /// Cached lookups keyed by file URL. `nil` value means "no tags
    /// found in the file" (distinct from "not yet probed", which is
    /// represented by the URL not being in the dictionary). The
    /// distinction matters because the loader skips re-probing
    /// URLs that already resolved to `nil` — saves a wasted file
    /// open on every scroll.
    @Published private(set) var entries: [URL: TrackMetadata?] = [:]

    private var inFlight: Set<URL> = []

    /// Request a metadata read for `url`. Idempotent — repeated
    /// calls before completion coalesce, and calls *after*
    /// completion return immediately without touching disk.
    func request(_ url: URL) {
        if entries[url] != nil { return }
        if inFlight.contains(url) { return }
        inFlight.insert(url)
        Task.detached(priority: .utility) { [weak self] in
            let result = readTrackMetadata(path: url.path)
            await self?.applyResult(url: url, result: result)
        }
    }

    private func applyResult(url: URL, result: TrackMetadata?) {
        entries[url] = result
        inFlight.remove(url)
    }

    /// Drop everything we've cached. Called on folder change so we
    /// don't keep megabytes of stale entries around as the user
    /// browses through a deep music tree.
    func reset() {
        entries.removeAll()
        inFlight.removeAll()
    }
}

/// Slim browser view bound to the app model. Tracks its own current
/// directory + listing; only the **selection** flows back into the
/// model so `Space` can load it.
struct FileBrowserView: View {

    @ObservedObject var model: WaveformAppModel

    @State private var currentDirectory: URL = FileBrowserView.defaultDirectory()
    @State private var entries: [BrowserEntry] = []
    /// M10.5r per-folder tag-metadata cache. Rebuilt on every
    /// folder change (see `reload()` + `currentDirectory` observer).
    @StateObject private var metadata = BrowserMetadataCache()
    /// M10.5d snappy-click bookkeeping. Stacking two `.onTapGesture`
    /// handlers (count: 1 + count: 2) forces SwiftUI to defer the
    /// single-tap handler until the system double-click interval
    /// expires — a noticeable lag (~250-500 ms, NSEvent's
    /// `doubleClickInterval`). We replace the double-click gesture
    /// with manual detection: the single-tap handler fires the
    /// select immediately and, if a second click on the same row
    /// lands within `doubleClickInterval`, also descends. Net
    /// effect: zero-lag select on every click, double-click
    /// folder-descent still works.
    @State private var lastClickURL: URL?
    @State private var lastClickAt: Date?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider().overlay(DubColor.divider)
            listing
        }
        .frame(minHeight: DubLayout.libraryMinHeight)
        .background(DubColor.surface1)
        .onAppear(perform: reload)
    }

    // MARK: - Header (folder path + picker)

    private var header: some View {
        HStack(spacing: DubSpacing.sm) {
            Text("LIBRARY")
                .font(DubFont.caps)
                .tracking(1.2)
                .foregroundStyle(DubColor.textSecondary)
            Divider().frame(height: 12).overlay(DubColor.divider)

            Button {
                goUp()
            } label: {
                Image(systemName: "arrow.up")
            }
            .disabled(currentDirectory.pathComponents.count <= 1)
            .help("Go up one folder")

            Text(currentDirectory.path)
                .font(DubFont.body)
                .foregroundStyle(DubColor.textPrimary)
                .lineLimit(1)
                .truncationMode(.head)

            Spacer(minLength: 0)

            Button("Choose folder…") {
                presentFolderPicker()
            }
            .controlSize(.small)
        }
        .padding(.horizontal, DubSpacing.lg)
        .padding(.vertical, DubSpacing.sm)
        .background(DubColor.surface2)
    }

    // MARK: - Listing

    @ViewBuilder
    private var listing: some View {
        if entries.isEmpty {
            emptyListing
        } else {
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(entries) { entry in
                        row(for: entry)
                    }
                }
                .padding(.vertical, DubSpacing.xs)
            }
        }
    }

    private var emptyListing: some View {
        VStack(spacing: DubSpacing.sm) {
            Spacer()
            Text("No audio files in this folder")
                .font(DubFont.body)
                .foregroundStyle(DubColor.textTertiary)
            Text("Use “Choose folder…” to navigate to your music.")
                .font(DubFont.micro)
                .foregroundStyle(DubColor.textPlaceholder)
            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private func row(for entry: BrowserEntry) -> some View {
        let isSelected = model.browserSelection == entry.url
        // M10.5b: do NOT wrap the row in a `Button`. `Button` claims
        // the primary press gesture before the drag recogniser can
        // install, which is why drag-out from this browser was
        // silently failing while Finder drag-in (no Button in the
        // chain) worked. Tapping is handled by `.onTapGesture`
        // *after* `.onDrag` so AppKit tries drag first and falls
        // through to tap when the pointer hasn't moved.
        let tags: TrackMetadata? = entry.isDirectory
            ? nil
            : (metadata.entries[entry.url] ?? nil)
        HStack(spacing: DubSpacing.md) {
            Image(systemName: entry.isDirectory ? "folder" : "waveform")
                .foregroundStyle(
                    entry.isDirectory
                        ? DubColor.textSecondary
                        : DubColor.textPrimary
                )
            VStack(alignment: .leading, spacing: 1) {
                Text(rowTitleText(entry: entry, tags: tags))
                    .font(DubFont.body)
                    .foregroundStyle(DubColor.textPrimary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let subtitle = rowSubtitleText(entry: entry, tags: tags) {
                    Text(subtitle)
                        .font(DubFont.micro)
                        .foregroundStyle(DubColor.textTertiary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            Spacer(minLength: 0)
            if !entry.isDirectory {
                Text(entry.ext)
                    .font(DubFont.micro)
                    .foregroundStyle(DubColor.textTertiary)
            }
        }
        .padding(.horizontal, DubSpacing.lg)
        .padding(.vertical, DubSpacing.xs)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(isSelected ? DubColor.surface2 : Color.clear)
        .contentShape(Rectangle())
        .onAppear {
            if !entry.isDirectory {
                metadata.request(entry.url)
            }
        }
        .if(!entry.isDirectory) { view in
            // AppKit drag path: NSItemProvider lets the OS use the
            // row's snapshot as the drag image, anchored under the
            // cursor at the mouse-down point. SwiftUI's
            // `.draggable(_:preview:)` instead renders the preview
            // closure at the row's layout position first and then
            // animates it toward the cursor, producing the
            // "fly-in from row to cursor" effect we want to avoid.
            view.onDrag {
                NSItemProvider(object: entry.url as NSURL)
            }
        }
        .onTapGesture {
            // Snappy single-click select. Fires immediately — no
            // wait for the system double-click timeout because we
            // do not declare a second `.onTapGesture(count: 2)`
            // (which would force SwiftUI to defer this handler).
            // Selection works on any row type; Space ignores
            // directories with a polite error.
            model.browserSelection = entry.url

            // Manual double-click detection: if the previous click
            // hit the same row inside the system double-click
            // interval, treat *this* click as the second half of
            // a double — folders descend, files are intentionally
            // non-double-clickable to load (per M10.5b).
            let now = Date()
            let interval = NSEvent.doubleClickInterval
            let isDouble = (lastClickURL == entry.url)
                && (lastClickAt.map { now.timeIntervalSince($0) < interval } ?? false)
            if isDouble, entry.isDirectory {
                currentDirectory = entry.url
                reload()
                lastClickURL = nil
                lastClickAt = nil
            } else {
                lastClickURL = entry.url
                lastClickAt = now
            }
        }
    }

    // MARK: - Filesystem helpers

    private func reload() {
        entries = Self.listing(at: currentDirectory)
        // M10.5r: drop the previous folder's tag cache so we don't
        // keep megabytes of strings around as the user browses
        // through a deep music tree. Per-row `onAppear` rebuilds
        // entries lazily as rows scroll back into view.
        metadata.reset()
    }

    /// Primary text for a browser row. **Files**: container-tag
    /// title when present, else the file's basename without the
    /// extension. **Folders**: the folder name. The fallback keeps
    /// the browser useful on untagged libraries (loose WAV stems,
    /// freshly-decoded YouTube rips, etc.).
    private func rowTitleText(entry: BrowserEntry, tags: TrackMetadata?) -> String {
        if entry.isDirectory {
            return entry.displayName
        }
        if let title = tags?.title, !title.isEmpty {
            return title
        }
        return entry.url.deletingPathExtension().lastPathComponent
    }

    /// Secondary text for a browser row. **Files** with a tag-derived
    /// artist render it; files without (untagged) render the original
    /// filename so the basename → title fallback in `rowTitleText`
    /// doesn't lose information. Folders render no subtitle.
    private func rowSubtitleText(entry: BrowserEntry, tags: TrackMetadata?) -> String? {
        if entry.isDirectory { return nil }
        if let artist = tags?.artist, !artist.isEmpty {
            // Album would crowd the row; we render artist only.
            return artist
        }
        // Untagged file: show the full filename as the secondary
        // line if it differs from the displayed title (e.g. the
        // basename was abbreviated for the title row).
        let basename = entry.displayName
        if basename != rowTitleText(entry: entry, tags: tags) {
            return basename
        }
        return nil
    }

    private func goUp() {
        let parent = currentDirectory.deletingLastPathComponent()
        if parent != currentDirectory {
            currentDirectory = parent
            reload()
        }
    }

    private func presentFolderPicker() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.directoryURL = currentDirectory
        panel.prompt = "Choose"
        if panel.runModal() == .OK, let url = panel.url {
            currentDirectory = url
            reload()
        }
    }

    private static func listing(at dir: URL) -> [BrowserEntry] {
        let fm = FileManager.default
        let opts: FileManager.DirectoryEnumerationOptions =
            [.skipsHiddenFiles, .skipsSubdirectoryDescendants, .skipsPackageDescendants]
        guard
            let items = try? fm.contentsOfDirectory(
                at: dir,
                includingPropertiesForKeys: [.isDirectoryKey],
                options: opts)
        else {
            return []
        }
        let mapped: [BrowserEntry] = items.compactMap { url -> BrowserEntry? in
            let values = try? url.resourceValues(forKeys: [.isDirectoryKey])
            let isDir = values?.isDirectory ?? false
            if isDir {
                return BrowserEntry(url: url, isDirectory: true)
            }
            let ext = url.pathExtension.lowercased()
            if kAudioExtensions.contains(ext) {
                return BrowserEntry(url: url, isDirectory: false)
            }
            return nil
        }
        // Folders first, then files; both sorted case-insensitively.
        return mapped.sorted { (lhs, rhs) in
            if lhs.isDirectory != rhs.isDirectory {
                return lhs.isDirectory && !rhs.isDirectory
            }
            return lhs.displayName.localizedCaseInsensitiveCompare(rhs.displayName)
                == .orderedAscending
        }
    }

    /// Default landing directory: `~/Music` if it exists, otherwise
    /// `~`. Most macOS users keep their library under Music; the
    /// fallback ensures the picker never opens onto a non-existent
    /// path.
    private static func defaultDirectory() -> URL {
        let fm = FileManager.default
        let home = fm.homeDirectoryForCurrentUser
        let music = home.appendingPathComponent("Music")
        var isDir: ObjCBool = false
        if fm.fileExists(atPath: music.path, isDirectory: &isDir), isDir.boolValue {
            return music
        }
        return home
    }
}

// MARK: - Conditional view modifier

private extension View {
    /// Apply `transform` only when `condition` is true. Used for
    /// conditional `.draggable` since SwiftUI's modifier signature
    /// would otherwise force us to apply `.draggable` to folder
    /// rows too.
    @ViewBuilder
    func `if`<Transform: View>(
        _ condition: Bool,
        transform: (Self) -> Transform
    ) -> some View {
        if condition {
            transform(self)
        } else {
            self
        }
    }
}

#Preview {
    FileBrowserView(model: WaveformAppModel())
        .frame(width: 1440, height: 360)
}
