# Plan: FlipGo virtual split display (software fallback)

Ticket folder (copied there at implementation start): `C:\projects\Personal\virtual-desktop-display\.claude\tickets\flipgo-virtual-split\plan.md`

## Context

The JSAUX FlipGo Lite is a stacked dual-panel monitor. Over one cable from the RTX 5070 Ti it shows only **one** stream: a tall 1920×2160 image across both panels. You get two independent panels only via DP MST over the USB-C "D" port. Tests showed the UGREEN cable doesn't pass MST, and the HDMI and DP inputs can't drive one panel each. An AGFINEST DP→USB-C adapter is ordered, but it may fail the same way.

This app is the fallback. It makes Windows see **two real 1920×1080 monitors**, so maximize, snapping and YouTube/game fullscreen each fill one panel. It does this with:

- two virtual monitors from an existing signed IddCx driver;
- a renderer that captures both and draws them top/bottom on the physical FlipGo in its tall mode.

Facts that shape the design:

- **Machine:** Win 11 **Pro** 26H2 (build 26300), Lenovo Legion T5 30IAS10, RTX 5070 Ti only (no iGPU), Asus VG34VQL3A primary 3440×1440, FlipGo-H/-A at 1920×2160 (150% scaling). Rust 1.99 MSVC + VS Build Tools 2022 are installed. No virtual display driver is installed yet. The repo is empty apart from `README.md`.
- **Specialized display is out.** "Remove display from desktop" with Windows.Devices.Display.Core is Enterprise / Pro for Workstations only, so the physical FlipGo stays in the desktop. We hide it behind our window and keep the cursor and windows off it.
- **User choices:** Rust (windows-rs) for the language, tray icon + TOML config for the controls.

## Approach

```
 Windows desktop                         FlipGo physical (1920x2160, tall mode)
 [V-top 1920x1080] ─WGC capture─┐      ┌───────────────────────┐
 [V-bot 1920x1080] ─WGC capture─┤ D3D11├─ top half  ← V-top    │  borderless topmost
 (parsec-vdd virtual monitors)  └──────┤─ bottom half ← V-bot  │  window, flip swapchain
                                        └───────────────────────┘
```

- **Virtual monitors: parsec-vdd** (signed Parsec IddCx driver, IOCTL API).
  - Displays exist only while the app sends a keep-alive UPDATE every 100 ms or less. If the app exits or crashes, they disappear in about 1 s and Windows returns to the normal layout. That makes it crash-safe.
  - 1920×1080 is a built-in mode, set with `ChangeDisplaySettingsExW`.
  - It sits behind a `VirtualDisplays` trait. The fallback backend is **MttVDD** (VirtualDrivers/Virtual-Display-Driver: `vdd_settings.xml` count=2, pipe `\\.\pipe\MTTVirtualDisplayPipe` `SETDISPLAYCOUNT n`), used if the parsec spike fails.
- **Capture: Windows.Graphics.Capture** per virtual monitor.
  - Uses `IGraphicsCaptureItemInterop::CreateForMonitor`, a free-threaded frame pool on **our** D3D11 device, `IsCursorCaptureEnabled = true` (the cursor is drawn in), and `IsBorderRequired = false` (best effort; reportedly honoured unpackaged on 24H2+).
  - Written by hand rather than with the `windows-capture` crate, so frames stay as GPU textures on one device (zero-copy).
- **Compose:** a flip-discard swapchain B8G8R8A8 sized to the physical mode. Each frame does `CopySubresourceRegion` of the latest V-top texture to (0,0) and V-bot to (0,1080), then `Present(1)`. No shaders are needed while the sizes match exactly; a stretch shader is added only if the physical mode differs. A waitable swapchain keeps latency to about 1 frame.
- **Physical display handling**, since the FlipGo stays in the desktop:
  - **Layout:** save the current topology, then put V-top/V-bot where the FlipGo was (left of the Asus, stacked) and move the physical FlipGo to touch only the Asus's bottom edge. Uses `QueryDisplayConfig`/`SetDisplayConfig`. The original layout is restored on stop.
  - **Cursor portal:** a `WH_MOUSE_LL` hook. If a move would land inside the physical rect, block it and `SetCursorPos` to the matching point on V-top/V-bot (top half → V-top, bottom half → V-bot), so the hidden screen acts as a mirror of the virtual ones. The hook runs on its own thread with a message loop and stays well under the 1 s timeout.
  - **Window rescue:** `SetWinEventHook` (`EVENT_OBJECT_SHOW`, `EVENT_SYSTEM_MOVESIZEEND`, `EVENT_SYSTEM_FOREGROUND`). Any visible top-level window that is not ours and whose `MonitorFromWindow` is the physical FlipGo gets moved to V-top with the same relative rect.
  - **Renderer window:** `WS_POPUP`, `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`, so it never takes focus or appears in Alt-Tab.
- **Identification:**
  - Physical: the monitor whose friendly name contains a config string (default `"FlipGo"`), via `DisplayConfigGetDeviceInfo` target name. This matches -H (HDMI) and -A (DP).
  - Virtual: the two new targets that appear after ADD, matched by the parsec adapter/monitor device path. Confirm the exact ID in the spike.
- **App shell:** a single-instance mutex and Per-Monitor-V2 DPI awareness (manifest), so we work in physical pixels; the FlipGo runs at 150%.
  - Tray menu: Start/Stop split, Swap top/bottom, Open config, Reload config, Start with Windows (HKCU Run), Exit.
  - Config is `%APPDATA%\virtual-desktop-display\config.toml`. Logging uses `tracing` to a rolling file next to the config.

## Project layout (single binary crate)

| Path | Purpose |
|---|---|
| `Cargo.toml` | `windows` 0.62 (features: Win32_Graphics_{Direct3D11,Dxgi,Gdi}, Win32_Devices_Display, Win32_UI_{WindowsAndMessaging,Shell,HiDpi,Accessibility}, Win32_System_{WinRT_Graphics_Capture,WinRT_Direct3D11,Threading,IO,Registry}, Graphics_Capture, Graphics_DirectX_Direct3D11, Foundation), `serde`, `toml`, `anyhow`, `tracing`, `tracing-subscriber`, `tracing-appender`; build-dep `embed-resource` |
| `build.rs`, `res/app.manifest`, `res/app.rc`, `res/icon.ico` | manifest (PerMonitorV2, `windowsSettings`), icon |
| `src/main.rs` | single instance, logging, tray message loop, wires `Session` start/stop |
| `src/config.rs` | `Config { physical_match, resolution, swap, refresh_hz, autostart, backend }` + defaults, load/save |
| `src/vdd/mod.rs` | `trait VirtualDisplays { fn add(n) -> Vec<Target>; fn remove_all(); }` |
| `src/vdd/parsec.rs` | open device interface `{00b41627-04c4-429e-a26e-0265cf50c8fa}`, IOCTLs ADD `0x0022e004` / REMOVE `0x0022a008` / UPDATE `0x0022a00c` / VERSION `0x0022e010`, keep-alive thread |
| `src/vdd/mtt.rs` | (only if needed) pipe-based backend |
| `src/displays.rs` | enumerate targets (name, device path, rect, HMONITOR), find physical/virtual, set mode, `Layout::compute()` (pure fn), apply/restore topology |
| `src/capture.rs` | WGC session per HMONITOR → `Arc<Mutex<Option<ID3D11Texture2D>>>` latest frame + event |
| `src/renderer.rs` | D3D11 device (RTX by LUID), window on physical rect, swapchain, compose loop thread |
| `src/cursor_portal.rs` | LL mouse hook + pure `map_point(physical_rect, top, bot, pt)` |
| `src/window_rescue.rs` | WinEvent hook + move logic |
| `src/session.rs` | start order: vdd add → wait for targets → set modes → layout → capture → renderer → hooks; stop in reverse + restore layout |
| `src/tray.rs`, `src/autostart.rs` | Shell_NotifyIconW menu; HKCU Run value |

## Phases

0. **Spike (go/no-go, about 1 h).**
   - Install the parsec-vdd driver (signed, `parsec-vdd-0.45.0.0.exe`).
   - `examples/vdd_probe.rs`: add 2 displays as a **non-admin** user, keep them alive for 30 s, and print the new targets and their device paths.
   - `examples/capture_probe.rs`: WGC one frame from a virtual monitor and save it as a PNG.
   - If the IOCTL needs admin or capture fails, switch to the MttVDD backend.
   - **Status:** probes written and compile. `capture_probe` RAN against the physical FlipGo-H: WGC capture OK (1920x2160 frame in ~1.5 ms, PNG verified visually). `vdd_probe` NOT RUN (driver not installed); non-admin IOCTL access still unverified.
1. **Core pipeline (CLI, Ctrl+C to stop):** `vdd` + `displays` + `capture` + `renderer` + `session`. YouTube fullscreen on V-top shows only on the top panel.
   - **Status:** code complete (src/{lib,main,config,displays,capture,renderer,session}.rs, vdd/{mod,parsec}.rs); build, clippy -D warnings, tests (6) pass. End-to-end session NOT RUN (no driver). Deviations: lib crate is named `vdd_app`; parsec port follows header (32-byte zero-padded input, DWORD output, 5 s overlapped wait; REMOVE index is 16-bit big-endian); `QueryDeviceStatus` not ported; assumes new virtual targets become active automatically.
2. **Cursor portal + window rescue.**
   - **Status:** implemented (`cursor_portal.rs`, `window_rescue.rs`, wired in `session.rs`); unit tests for `map_point` and `rescue_rect` pass; NOT run end-to-end.
3. **Tray, config, autostart, logging, single instance, manifest/icon.**
   - **Status:** implemented (`tray.rs`, `autostart.rs`, `build.rs`, `res/`, config `autostart`/`start_on_launch`/`save`, file logging, mutex `Local\VirtualDesktopDisplay`); release build embeds manifest + icon; NOT run end-to-end. `--seconds N` implies `--cli`.
4. **Robustness:**
   - `WM_DISPLAYCHANGE`: rebuild if the physical display disappears, pause if the FlipGo is unplugged.
   - Sleep/resume: re-add displays.
   - Swap halves; stretch shader if the physical mode isn't 1920×2160.
   - README with setup steps.
   - **Status:** (a) layout applied without SDC_SAVE_TO_DATABASE (needs e2e confirmation), leftover-layout warning at start; (b) IOCTL timing logged, wait logic matches the header, ADD latency looks driver-side; (c) WM_DISPLAYCHANGE health check (debounced) stops the session + balloon; (d) resume restarts a running session; (e) live swap via `Arc<AtomicBool>`; (f) shader stretch path per half; (g) README written. Also: new targets are detected via QDC_ALL_PATHS and explicitly activated (auto-wait, then SDC_TOPOLOGY_EXTEND, then manual path injection); stop removes the virtual displays BEFORE restoring the layout. NOT run end-to-end.

Commit after each phase on a branch `flipgo-virtual-split` (the repo is on `master`, remote `Tihi321/virtual-desktop-display`).

## Verification

- `cargo build --release`, `cargo clippy -- -D warnings`, and `cargo test`. The tests cover pure logic: `Layout::compute` for the current monitor set, `map_point` edge cases (exact boundaries, top/bottom split at y=1080), and config default/parse round-trip.
- Manual end-to-end test on this machine (FlipGo on HDMI tall mode, charger in the plug port):
  1. Start from the tray. Settings → Display shows 4 displays; Identify shows the virtual numbers on the correct FlipGo panels.
  2. YouTube fullscreen (F) on the top panel stays on the top panel. Same on the bottom.
  3. Maximize and Win+←/→ snapping work per panel.
  4. Move the mouse from the Asus to both panels and back; the cursor never gets stuck on the hidden physical display. Also check after a UAC prompt and after lock/unlock.
  5. Open a new app while the physical display is "focused" (e.g. via Win+Shift+arrow). It gets rescued onto V-top.
  6. Stop from the tray: the layout returns to the original. Kill the process from Task Manager: the virtual displays vanish within about 2 s and the FlipGo shows the desktop again.
  7. Measure latency with a stopwatch page in a photo of both screens; target is about 1 frame (≈16–33 ms). CPU < 3%, GPU low in Task Manager.
- Known limitations to document: DRM video (Netflix) shows black; about 1 frame of lag (fine for video and work, not ideal for competitive games); no touch.
