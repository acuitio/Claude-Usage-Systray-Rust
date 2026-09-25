# Claude Usage Systray (Rust)

Pure Rust + Win32 port of [Claude-Usage-Systray-CSharp](https://github.com/paulmah79/Claude-Usage-Systray-CSharp).
Drops the .NET runtime entirely. The published `.exe` is **~130 KB**,
self-contained, no runtime install required on the target machine.

## Status

**Feature-complete vs. the C# port**, plus the imgpaste feature below
which doesn't exist in the C# or Python siblings. One known gap:
per-window AppUserModelID grouping under one taskbar button — skipped
because it requires `IPropertyStore` COM interop, which is rough in
`windows-sys`.

What's implemented:
- Tray icon with right-click menu (Dashboard / Overlay toggle / Refresh
  Now / Usage Chart / Settings / **Send Clipboard Image** / Quit),
  checkmarks on toggleable items.
- Dashboard window — real plan + cache age + three usage bars with
  per-bar reset times derived from ISO timestamps.
- Overlay — layered window, per-pixel alpha, drag-to-move, position
  persisted to `config.json`, rounded corners, a 1px opacity-aware
  border, and a two-pass drop shadow (wide glow + tight core via the
  GDI+ Blur effect) for legibility against translucent backdrops.
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
- `WM_DPICHANGED` handler for moves between monitors.
- Windows 11 Settings → Other system tray icons self-patch (registry).
- Startup-on-login via `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
- **imgpaste — `Alt+Shift+V` uploads the clipboard image over SCP and
  pastes the resulting remote path into the focused window.** See
  the "imgpaste" section below.
- **Self-update:** checks public GitHub Releases for a newer build (~20 s after
  launch, then every 30 min, and on "Refresh Now"), downloads the matching-arch
  artifact, swaps the running exe in place, and relaunches. Window
  positions survive because config lives beside the exe. See the
  "Self-update" section below.

## imgpaste — clipboard image over SSH

When you press `Alt+Shift+V` (or pick "Send Clipboard Image" from the
tray menu), the app:

1. Reads the bitmap currently on the Windows clipboard.
2. Saves it to `%TEMP%\imgpaste-<unix_ns>.png` via GDI+.
3. Shells out to `scp.exe` to upload it as
   `<host>:<remote_dir>/imgpaste-<unix_secs>.png`.
4. Writes the resulting remote path back to the clipboard as text and
   synthesises `Ctrl+V` against the focused window.
5. Restores the user's original clipboard image afterwards.

The intended use case: paste a screenshot into a `claude` CLI session
running over SSH. The keystrokes carry the *path* through your existing
terminal; SCP carries the *bytes* through a separate transient SSH
session; the two converge when Claude Code opens the file by path.

Config lives in `app_state.json`:

| Field | Default | Notes |
|---|---|---|
| `imgpaste_enabled` | `true` | Set `false` to skip hotkey registration |
| `imgpaste_host` | `"gx10"` | Any `ssh`-resolvable host (`~/.ssh/config` honored) |
| `imgpaste_remote_dir` | `"/tmp"` | Created on demand by the remote side |
| `imgpaste_hotkey_mods` | `0x0005` | `MOD_ALT \| MOD_SHIFT` |
| `imgpaste_hotkey_vk` | `0x56` | `'V'` |

Env vars override config at startup for ad-hoc one-shots:
`IMGPASTE_HOST`, `IMGPASTE_REMOTE_DIR`.

**Prerequisites on the host running this app:**
- OpenSSH client (provides `scp.exe`). Built into Windows 11; on
  Windows 10 add it via `Settings → Apps → Optional features →
  OpenSSH Client`.
- Working passwordless SSH to the configured host (key in
  `~/.ssh/authorized_keys` on the remote, or `ssh-agent` loaded).
- Failures are logged to `imgpaste.log` in the app's config dir;
  they never crash the tray.

See [BUILD.md](BUILD.md) for how to build and run.

## Self-update

The app keeps itself current from public GitHub Releases with no manual download.
CI publishes a Release on each green push to `main`. A background
thread checks ~20 s after launch and every 30 minutes; **"Refresh Now" (tray
menu) triggers an immediate check** alongside the usage refresh:

1. Asks GitHub anonymously for the latest Release and compares its commit SHA
   against this binary's embedded build SHA.
2. If newer, downloads the Release asset for *this* architecture, validates it
   (plausible size + `MZ` PE header), and stages it beside the exe.
3. Renames the running exe aside (`<exe>.old`), moves the new one into place,
   relaunches, then the fresh process deletes the leftover `.old` on boot.

The swap + relaunch happen on the UI thread so the global hotkeys and tray
icon are released before the new instance claims them. `app_state.json`
(window positions, colours, hotkeys) lives beside the exe and is never
touched, so your layout survives an update.

Config in `app_state.json`:

| Field | Default | Notes |
|---|---|---|
| `auto_update` | `true` | Master switch. Set `false` (or untick "Automatically install updates" in Settings) to disable both the periodic check and the "Refresh Now" check. |

**Prerequisites:**
- Local/dev builds (compiled without CI) carry no build SHA and never
  self-update, so a local checkout won't clobber itself with a CI artifact.
- Releases are public and the updater needs no GitHub login, token, or `gh`
  installation. Failed network checks are logged no-ops, so the app is never
  blocked or broken by an unavailable release.

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
├── settings.rs    — controls dialog (BUTTON / COMBOBOX / EDIT / UPDOWN
│                    + ChooseColor)
├── imgpaste.rs    — clipboard image → SCP upload → SendInput(Ctrl+V)
└── update_service.rs: self-update from public Releases, download, in-place swap, relaunch
```
(Plus `poll_service`, `usage_fetcher`, `oauth_refresh`, `health`, `imgpull`,
`chart`, `state_store`, and the `build.rs` that embeds the build SHA.)

## Known limitations

- **No per-window AppUserModelID grouping.** Dashboard + Settings + Chart
  each show as separate taskbar buttons rather than grouping under one
  identity. Fix needs `IPropertyStore` + `SHGetPropertyStoreForWindow`
  COM interop, which is rough in `windows-sys` without the typed
  wrappers. Cosmetic.
- **No scrollable Settings.** Dialog is sized to fit everything on
  typical screens (~1100 px tall) and is sizable/maximizable. If your
  display is shorter, drag the dialog to access the lower controls.

## Future ideas

- **Acrylic / frosted-glass background for the overlay.** Swap the
  `WS_EX_LAYERED` + `UpdateLayeredWindow` per-pixel-alpha pipeline for
  a regular DWM-composited window with `DwmEnableBlurBehindWindow` or
  `SetWindowCompositionAttribute(WCA_ACCENT_POLICY)` so the overlay
  sits on a live-blurred backdrop instead of a solid translucent
  rectangle — the look Windows 11 Settings / Files use. Substantial
  refactor: the entire render-to-DIB-then-push pattern goes away, drag
  handling needs to switch to a normal `WM_NCHITTEST` flow, and the
  underlying API is technically undocumented (stable, but Microsoft
  tweaks behavior between Windows builds).

## Build numbers (release, AOT-equivalent)

CI produces both architectures on every push to `main`:

| Target | Triple | Binary size | Runtime RAM (idle) |
|---|---|---|---|
| Intel/AMD 64-bit | `x86_64-pc-windows-msvc` | ~885 KB | ~13 MB |
| ARM64 | `aarch64-pc-windows-msvc` | ~849 KB | ~13 MB |

Single binary in each case — no .NET runtime, no VC runtime, no
WebView2 prerequisite (downloaded on first launch if not already
present). Copy and run.

Note: the C# port ships a separate `ClaudeUsageCollector.exe` for
headless polling when the tray isn't running. The Rust port skips it
intentionally — the tray app polls on its own timer when it's up, and
since startup-on-login is wired, "tray is up" is the steady state.
The `background_collection` field stays in `AppConfig` for compatibility
with the C# version's `config.json` but has no effect.

## License

MIT.
