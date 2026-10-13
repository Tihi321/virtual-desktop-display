# virtual-desktop-display

Splits a stacked dual-panel monitor (JSAUX FlipGo) that Windows sees as **one tall 1920x2160 screen** into **two real 1920x1080 monitors**, so maximize, snapping and fullscreen video each fill exactly one panel.

How it works: two virtual monitors are created with the signed Parsec VDD (IddCx) driver, each is captured with Windows.Graphics.Capture, and a borderless topmost window on the physical FlipGo draws them top and bottom with a D3D11 flip-model swapchain (about 1 frame of latency). A low-level mouse hook keeps the cursor off the hidden physical screen and a WinEvent hook moves windows that land on it to the top virtual monitor.

## Requirements

- Windows 10/11, a GPU with D3D11.
- [Parsec Virtual Display Driver 0.45](https://builds.parsec.app/vdd/parsec-vdd-0.45.0.0.exe) installed (signed, no test mode needed).
- The FlipGo connected in its tall single-screen mode (HDMI or DP). Its Windows monitor name must contain `FlipGo` (configurable).
- Rust 1.80+ and the MSVC build tools to build from source.

## Build and run

```
cargo build --release
target\release\virtual-desktop-display.exe        # tray mode
target\release\virtual-desktop-display.exe --cli  # console mode, Ctrl+C to stop
target\release\virtual-desktop-display.exe --seconds 30   # console mode, stops itself
```

Only one instance runs at a time. Logs go to `%APPDATA%\virtual-desktop-display\logs\` (daily files; set `RUST_LOG=debug` for more).

## Tray menu

- **Start split / Stop split**: also toggled by double-clicking the icon.
- **Swap top/bottom**: switches which virtual monitor is drawn on which panel, live. Saved to the config.
- **Open config**: opens `config.toml` in Notepad (created with defaults if missing).
- **Reload config**: re-reads the file and restarts the split if it is running.
- **Start with Windows**: adds or removes the HKCU `Run` entry `VirtualDesktopDisplay`.
- **Exit**.

The split is stopped automatically (layout restored, balloon shown) if the FlipGo or a virtual monitor disappears. After sleep/resume a running split is torn down and started again.

## Config

`%APPDATA%\virtual-desktop-display\config.toml` (all keys optional):

| key | default | meaning |
|---|---|---|
| `physical_match` | `"FlipGo"` | substring of the physical monitor's friendly name |
| `width`, `height` | `1920`, `1080` | mode of each virtual monitor |
| `refresh_hz` | `60` | refresh rate of the virtual monitors |
| `swap` | `false` | draw the second virtual monitor on the upper panel |
| `autostart` | `false` | start with Windows (synced to the Run key on launch and reload) |
| `start_on_launch` | `true` | start the split as soon as the tray app launches |

If the physical size is not `width` x `2*height`, each half is stretched with a linear-filtered shader.

## Layout

While running, the virtual monitors take the physical FlipGo's former place (stacked) and the physical FlipGo is moved to touch only the bottom edge of the primary monitor, right-aligned. The layout is applied without `SDC_SAVE_TO_DATABASE`, so if the process is killed the virtual monitors vanish within about a second and Windows falls back to its stored layout. On a normal stop the virtual monitors are removed first and the original layout is then re-applied.

## Known limitations

- DRM-protected video (for example Netflix in a browser) is black in the capture.
- About one frame of added latency: fine for video and work, not for competitive games.
- No touch input forwarding.
- Cursor portal: the cursor cannot rest on the physical screen. Moving onto it warps the cursor to the matching point on the top (upper half) or bottom (lower half) virtual monitor. Elevated windows (UAC prompts) can bypass the hook while they are focused.
- Window rescue moves new, dragged or focused windows on the physical monitor to the top virtual monitor (keeping their relative position, clamped to fit); tool windows and the taskbar are left alone.
- The physical monitor stays part of the desktop (Windows cannot detach it on Pro editions); it is just covered by the renderer window.
- Manual rearrangement of displays in Settings while the split runs is not tracked.
