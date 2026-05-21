# Build & Run

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

## Cross-compile (future)

`win-x64` is the default target on Windows. ARM64 Windows is:

```powershell
rustup target add aarch64-pc-windows-msvc
cargo build --release --target aarch64-pc-windows-msvc
```

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
