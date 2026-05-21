# Claude Usage Systray (Rust)

Pure Rust + Win32 port of [Claude-Usage-Systray-CSharp](https://github.com/paulmah79/Claude-Usage-Systray-CSharp).
Drops the .NET runtime entirely. The published `.exe` is **~130 KB**,
self-contained, no runtime install required on the target machine.

## Status

**Prototype-stage.** The Win32 UI scaffolding is in place — tray icon
with context menu, overlay, dashboard, settings — but the data layer
(HTTP to the Anthropic usage API, OAuth refresh, config persistence,
cooldown logic, history+chart) hasn't been ported yet.

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

## Roadmap

1. **Data layer** — port `UsageFetcher`, `OAuthRefresh`, `CooldownStore`,
   `UsageCacheStore`, `UsageHistoryStore`, `CredentialsStore` from the
   C# project. HTTP via `reqwest`, JSON via `serde_json`, atomic writes
   via `atomicwrites`.
2. **Config persistence** — same `config.json` schema as the C# version
   so the two can coexist during cutover.
3. **OAuth refresh** — `EnsureFreshAsync` equivalent, with the same
   401 → cooldown cascade.
4. **Polling service** — timer-driven fetch, mirrors `PollService.cs`.
5. **Chart window** — drawn with raw GDI (history line plot).
6. **Settings polish** — scrollable container, font picker, all the
   colour rows and `show_*` checkboxes.
7. **Tray registry self-patch** — port `TrayRegistryPatch.cs` so the
   icon shows in Windows 11 Settings → Other system tray icons.
8. **Background collector** — port `Collector` project (headless poll).

## License

MIT.
