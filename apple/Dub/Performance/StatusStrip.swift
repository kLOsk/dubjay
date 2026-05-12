//
//  StatusStrip.swift
//  Dub
//
//  M10.3 thin status strip at the top of the Performance View.
//  M10.4 — fills in two of the M10.3 placeholders:
//      • CLOCK: wall clock (HH:MM, no seconds — see PRD §9.3 for why
//        we don't tick at 1 Hz).
//      • BATTERY: macOS power-source indicator. Warns visually below
//        20 %; **never blocks or pauses playback** (a low-battery
//        modal in a club is a career-ending UX failure).
//
//  Stateless — pure function of `StatusStripState`. The driver
//  (`StatusStripContainer`) refreshes the clock + battery on a
//  timer and re-renders.
//
//  See PRD §9.3 for the design intent (read-only contract, no
//  interactive elements).
//

import SwiftUI
import IOKit.ps

struct StatusStripState: Equatable {
    let engineVersion: String
    let sampleRate: UInt32
    let isRunning: Bool
    /// Local wall-clock string, formatted `HH:mm` (24-hour) by the
    /// driver. `nil` if the driver hasn't sampled yet.
    let clockText: String?
    /// Snapshot of the system power source, polled by the driver
    /// at ~1 Hz. `nil` means "unknown" and the strip renders
    /// nothing (rather than a misleading "100 %" or "0 %").
    let power: PowerState?

    /// Sample rate formatted as "48.0 kHz" or `nil` if `0` (engine
    /// not running yet).
    var sampleRateText: String? {
        guard sampleRate > 0 else { return nil }
        let khz = Double(sampleRate) / 1000.0
        return String(format: "%.1f kHz", khz)
    }
}

/// Lightweight power-source snapshot. Built from
/// `IOPSCopyPowerSourcesInfo` — see `PowerSourcePoller`.
struct PowerState: Equatable {
    /// True when the laptop is plugged in (AC power).
    let isCharging: Bool
    /// Battery state-of-charge, 0...100. Clamped at the boundaries.
    let percent: Int
    /// True when the battery is below 20 %. Drives the amber tint
    /// + (M18) attention pulse.
    var isLow: Bool { !isCharging && percent < 20 }
}

struct StatusStrip: View {

    let state: StatusStripState

    var body: some View {
        HStack(spacing: DubSpacing.lg) {
            wordmark
            Divider()
                .frame(height: 12)
                .overlay(DubColor.divider)
            statusText
            Spacer(minLength: 0)
            clockView
            batteryView
        }
        .padding(.horizontal, DubSpacing.lg)
        .frame(height: DubLayout.statusStripHeight)
        .background(DubColor.surface1)
    }

    private var wordmark: some View {
        Text("DUB")
            .font(DubFont.display)
            .tracking(1.5)
            .foregroundStyle(DubColor.textPrimary)
    }

    @ViewBuilder
    private var statusText: some View {
        if state.isRunning {
            HStack(spacing: DubSpacing.sm) {
                Circle()
                    .fill(DubColor.stateLocked)
                    .frame(width: 6, height: 6)
                if let sr = state.sampleRateText {
                    Text(sr)
                        .font(DubFont.caps)
                        .tracking(0.6)
                        .foregroundStyle(DubColor.textSecondary)
                }
            }
        } else {
            HStack(spacing: DubSpacing.sm) {
                Circle()
                    .fill(DubColor.textPlaceholder)
                    .frame(width: 6, height: 6)
                Text("IDLE · ⌘, TO CONFIGURE")
                    .font(DubFont.caps)
                    .tracking(0.6)
                    .foregroundStyle(DubColor.textTertiary)
            }
        }
    }

    /// Wall-clock readout. Renders the cached string from the
    /// driver verbatim; no formatting decisions here so the
    /// view stays a pure function.
    @ViewBuilder
    private var clockView: some View {
        if let text = state.clockText {
            Text(text)
                .font(DubFont.caps)
                .tracking(0.8)
                .foregroundStyle(DubColor.textSecondary)
                .monospacedDigit()
        }
    }

    /// Battery readout. Plugged-in: dim power-plug glyph + percent.
    /// On battery: dim "🔋 NN %"; below 20 %, amber (PRD §9.3 —
    /// warns only, never blocks playback).
    @ViewBuilder
    private var batteryView: some View {
        if let power = state.power {
            HStack(spacing: DubSpacing.xs) {
                Image(systemName: batteryGlyph(for: power))
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(batteryTint(for: power))
                Text("\(power.percent)%")
                    .font(DubFont.caps)
                    .tracking(0.6)
                    .foregroundStyle(batteryTint(for: power))
                    .monospacedDigit()
            }
        }
    }

    private func batteryGlyph(for power: PowerState) -> String {
        if power.isCharging {
            return "bolt.fill"
        }
        switch power.percent {
        case ...20:  return "battery.25"
        case ...50:  return "battery.50"
        case ...85:  return "battery.75"
        default:     return "battery.100"
        }
    }

    private func batteryTint(for power: PowerState) -> Color {
        if power.isLow {
            return DubColor.stateTentative   // amber: visible warning
        }
        return DubColor.textSecondary
    }
}

// MARK: - Live driver

/// Wraps a `StatusStrip` and keeps its `clockText` + `power` state
/// refreshed on a 1 Hz timer. Pulled out as a separate view so the
/// stateless `StatusStrip` stays trivially previewable + snapshot-
/// testable in M18.
struct StatusStripContainer: View {

    let engineVersion: String
    let sampleRate: UInt32
    let isRunning: Bool

    @State private var clockText: String? = nil
    @State private var power: PowerState? = nil

    /// 1 Hz refresh covers both wall clock (display granularity is
    /// HH:mm, but a 60 s timer would let the minute hand lag by up
    /// to a minute at startup) and battery (drift at this cadence is
    /// imperceptible). `Timer.publish` runs on the main run-loop so
    /// updates land on the main actor.
    private let tick = Timer.publish(every: 1.0, on: .main, in: .common).autoconnect()

    var body: some View {
        StatusStrip(state: StatusStripState(
            engineVersion: engineVersion,
            sampleRate: sampleRate,
            isRunning: isRunning,
            clockText: clockText,
            power: power))
        .onAppear(perform: refresh)
        .onReceive(tick) { _ in refresh() }
    }

    private func refresh() {
        clockText = Self.clockFormatter.string(from: Date())
        power = PowerSourcePoller.snapshot()
    }

    /// 24-hour HH:mm — see PRD §9.3 (configurable to 12-hour in
    /// Preferences as a v1.x polish; not in M10.4 scope).
    private static let clockFormatter: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "HH:mm"
        return f
    }()
}

// MARK: - Power-source poller

/// Reads the macOS power-source list through `IOPSCopyPowerSourcesInfo`
/// and reduces it to a `PowerState`. Returns `nil` on Macs without a
/// battery (Mac mini, Studio, Pro) so the strip simply hides the
/// indicator rather than rendering misleading "100 % charged" for a
/// desktop on AC.
enum PowerSourcePoller {

    static func snapshot() -> PowerState? {
        guard let blob = IOPSCopyPowerSourcesInfo()?.takeRetainedValue(),
              let sources = IOPSCopyPowerSourcesList(blob)?.takeRetainedValue() as? [CFTypeRef]
        else {
            return nil
        }
        for source in sources {
            guard let desc = IOPSGetPowerSourceDescription(blob, source)?
                    .takeUnretainedValue() as? [String: Any] else { continue }
            // Filter to internal battery only — UPS power sources
            // share the same API but report semantics we don't
            // care about for the laptop-on-stage scenario.
            let type = desc[kIOPSTypeKey] as? String
            guard type == kIOPSInternalBatteryType else { continue }

            guard let current = desc[kIOPSCurrentCapacityKey] as? Int,
                  let capacity = desc[kIOPSMaxCapacityKey] as? Int,
                  capacity > 0
            else {
                continue
            }
            let raw = Int((Double(current) / Double(capacity)) * 100.0)
            let pct = Swift.max(Swift.min(raw, 100), 0)
            let powerSourceState = desc[kIOPSPowerSourceStateKey] as? String
            let isCharging =
                (powerSourceState == kIOPSACPowerValue)
                || ((desc[kIOPSIsChargingKey] as? Bool) == true)
            return PowerState(isCharging: isCharging, percent: pct)
        }
        return nil
    }
}

#Preview("idle") {
    StatusStrip(state: StatusStripState(
        engineVersion: "0.0.1", sampleRate: 0, isRunning: false,
        clockText: "21:47",
        power: PowerState(isCharging: false, percent: 87)))
        .frame(width: 1440)
}

#Preview("running 48 kHz, low battery") {
    StatusStrip(state: StatusStripState(
        engineVersion: "0.0.1", sampleRate: 48000, isRunning: true,
        clockText: "23:12",
        power: PowerState(isCharging: false, percent: 14)))
        .frame(width: 1440)
}

#Preview("running 48 kHz, plugged in") {
    StatusStrip(state: StatusStripState(
        engineVersion: "0.0.1", sampleRate: 48000, isRunning: true,
        clockText: "23:12",
        power: PowerState(isCharging: true, percent: 100)))
        .frame(width: 1440)
}
