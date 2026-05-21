# Claude Usage Systray (Rust)

Pure Rust + Win32 port of [Claude-Usage-Systray-CSharp](https://github.com/paulmah79/Claude-Usage-Systray-CSharp).
Drops the .NET runtime entirely. The published `.exe` is **~130 KB**,
self-contained, no runtime install required on the target machine.

## Status

**Feature-complete vs. the C# port** (one known gap: per-window
AppUserModelID grouping under one taskbar button — skipped because it
requires `IPropertyStore` COM interop, which is rough in `windows-sys`).

What's implemented:
- Tray icon with right-click menu (Dashboard / Overlay toggle / Refresh
  Now / Usage Chart / Settings / Quit), checkmarks on toggleable items.
- Dashboard window — real plan + cache age + three usage bars with
  per-bar reset times derived from ISO timestamps.
- Overlay — layered window, per-pixel alpha, drag-to-move, position
  persisted to `config.json`.
- Settings — every `AppConfig` field, including the 5 colour pickers,
  font family combo, opacity/scale spinners, refresh-mode combo, and
  all `show_*` checkboxes. Atomic write on Apply.
- Chart window — all three series on one canvas, GDI line plot, "MM/DD HH:MM"
  axis labels when span exceeds a day.
- HTTP layer — OAuth refresh against console.anthropic.com (fallback
  claude.ai), 60 s throttle, atomic write to credentials. Usage fetcher
  with cache-first / cooldown-aware / retry-with-backoff against the
  same `api.anthropic.com/api/oauth/usage` endpoint as the C# port.
- Background polling thread + `PostMessage(WM_USAGE_UPDATED)` ↔ UI.
- Headless `ClaudeUsageCollector.exe` second binary, spawned from the
  tray app when `background_collection` is enabled.
- `WM_DPICHANGED` handler for moves between monitors.
- Windows 11 Settings → Other system tray icons self-patch (registry).
- Startup-on-login via `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

See [BUILD.md](BUILD.md) for how to build and run.

## Why a Rust rewrite?

The C# version (.NET 10 + WinForms + NativeAOT) ships as a **~19 MB**
self-contained `.exe`. Rust + Win32 gives us:

- **~150× smaller exe** (~130 KB vs ~19 MB)
- **~3.5× less runtime RAM** (~10 MB vs ~32 MB)
- **No .NET runtime** — nothing to install, nothing to version-pin
- **Memory safety** — borrow checker replaces what GC was doing
- **Smaller surface area** for security review (no BCL, no JIT)
- **Comparable LOC** — the C# App layer is ~1,475 LOC; the Rust
  prototype is ~875 for the same UI surface (macros + raw layout
  drop framework boilerplate)

The cost is a third language to maintain alongside the C# version and
the Python original.

## Project layout

```
src/
├── main.rs        — entry, hidden tray-host window, message loop
├── common.rs      — palette, fonts, layout helpers, dummy data
├── tray.rs        — Shell_NotifyIcon + runtime-drawn icon + popup menu
├── overlay.rs     — WS_EX_LAYERED + UpdateLayeredWindow (per-pixel alpha)
├── dashboard.rs   — owner-drawn window with three usage bars
└── settings.rs    — controls dialog (BUTTON / COMBOBOX / EDIT / UPDOWN
                    + ChooseColor)
```

## Known limitations

- **No per-window AppUserModelID grouping.** Dashboard + Settings + Chart
  each show as separate taskbar buttons rather than grouping under one
  identity. Fix needs `IPropertyStore` + `SHGetPropertyStoreForWindow`
  COM interop, which is rough in `windows-sys` without the typed
  wrappers. Cosmetic.
- **Overlay rendering quality.** Pure GDI text on a per-pixel-alpha
  bitmap aliases against the translucent background. The C# port has
  the same limitation; the Python original uses a two-pass shadow
  pipeline that hasn't been ported to either.
- **No scrollable Settings.** Dialog is sized to fit everything on
  typical screens (~1100 px tall) and is sizable/maximizable. If your
  display is shorter, drag the dialog to access the lower controls.

## Build numbers (release, AOT-equivalent)

- `ClaudeUsageSystray.exe`: **1.51 MB**
- `ClaudeUsageCollector.exe`: **1.38 MB**
- Total source: 2,512 LOC across 20 files
- Runtime RAM (tray app idle): ~13 MB

## License

MIT.
