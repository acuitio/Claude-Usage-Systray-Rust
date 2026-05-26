# Build & Run

## Quickest path: download prebuilt binaries from CI

You don't need to install anything if you only want to run the app.
Every push to `main` produces both an x64 and an ARM64 `.exe` as a
GitHub Actions workflow artifact. Grab the latest:

```powershell
gh run list --workflow=ci.yml --branch=main --limit=1
# note the run id from the first column, then:
gh run download <run-id> --dir dist
```

You'll get two directories named
`ClaudeUsageSystray-x86_64-pc-windows-msvc-<sha>/` and
`ClaudeUsageSystray-aarch64-pc-windows-msvc-<sha>/`, each containing a
single `ClaudeUsageSystray.exe`. Pick the one matching your CPU
architecture. To check your machine's architecture:
`(Get-WmiObject Win32_Processor).Architecture` (9 = x64, 12 = ARM64).

The rest of this document is for building locally.

## Prerequisites

```powershell
# Rust toolchain (stable)
winget install Rustlang.Rustup

# Visual Studio 2022 Build Tools with the C++ workload (provides MSVC
# v143 + the linker that rustc invokes for the Windows target).
winget install Microsoft.VisualStudio.2022.BuildTools --silent `
  --accept-package-agreements --accept-source-agreements `
  --override "--quiet --norestart --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 --add Microsoft.VisualStudio.Component.Windows11SDK.22621"
```

Verify:

```powershell
rustc --version    # expect 1.80+
cargo --version
```

## Build

```powershell
# Dev (fast compile, slightly larger debug exe)
cargo build

# Release (small self-contained exe, ~130 KB)
cargo build --release
```

Output: `target\release\ClaudeUsageSystray.exe`. Copy it anywhere and run —
no .NET or VC runtime required (the C runtime is statically linked).

## Run

```powershell
.\target\release\ClaudeUsageSystray.exe
```

Right-click the tray icon for Dashboard / Overlay (toggle) / Refresh
Now / Settings / Quit.

## Cross-compile to ARM64

CI already produces ARM64 binaries on every push to `main` — see
"Quickest path" at the top. To build ARM64 locally:

```powershell
rustup target add aarch64-pc-windows-msvc
cargo build --release --target aarch64-pc-windows-msvc
```

Output lands at
`target\aarch64-pc-windows-msvc\release\ClaudeUsageSystray.exe` (note
the extra path segment when `--target` is explicit). You can build this
from an x64 machine — MSVC's linker emits ARM64 PE binaries
cross-arch. The resulting `.exe` runs natively on ARM64 Windows (Surface
Pro X, Copilot+ PCs) without the x64-on-ARM emulator penalty.

The x64 build also runs on ARM64 Windows via Microsoft's emulator, just
slower and more battery-hungry. For a long-lived tray app, prefer the
native build when running on ARM hardware.

## Why so many Win32 features in Cargo.toml?

`windows-sys` is feature-gated — each feature flag pulls in a different
slice of the Win32 API headers. We use:

| Feature | What we use it for |
|---|---|
| `Win32_Foundation` | `HWND`, `LPARAM`, `RECT`, basic types |
| `Win32_UI_WindowsAndMessaging` | Windows, messages, dialogs |
| `Win32_UI_Controls` | Common controls (UpDown, message constants) |
| `Win32_UI_Controls_Dialogs` | `ChooseColor` system dialog |
| `Win32_UI_HiDpi` | Per-monitor DPI awareness |
| `Win32_UI_Input_KeyboardAndMouse` | `SetCapture` / `ReleaseCapture` (drag) |
| `Win32_UI_Shell` | `Shell_NotifyIcon` for the tray |
| `Win32_Graphics_Gdi` | Drawing, brushes, fonts |
| `Win32_System_LibraryLoader` | `GetModuleHandle` |
| `Win32_System_Registry` | `RegSetValueExW` for the tray-icon self-patch |
| `Win32_Security` | (reserved for future ACL work on the credentials file) |
