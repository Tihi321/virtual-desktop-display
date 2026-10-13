# Virtual Desktop Display

Split any monitor into several **real virtual monitors** on Windows: columns, rows, a grid or custom ratios. Each piece shows up in Windows (and in Teams, Zoom, OBS, games, the taskbar) as its own display, while the physical monitor keeps showing the combined picture. It uses the Parsec Virtual Display Driver to create the virtual displays.

## Why

- **Share half of an ultrawide.** Cut a 3440x1440 monitor into two 1720x1440 displays and share just one of them in Teams, Zoom or OBS.
- **Fullscreen video in one half.** Watch a fullscreen video on one half while you work on the other.
- **Stacked dual-panel portable monitors on a single cable**, for example the JSAUX FlipGo: Windows sees one tall display, this tool turns it into two normal 1920x1080 monitors.
- **A 4K TV as a 2x2 grid** of four 1080p monitors.

## How it works

1. The app asks the Parsec VDD driver for virtual displays, one per piece, each sized to its piece.
2. Each virtual display is captured with Windows.Graphics.Capture and drawn onto the matching part of the real display.
3. The real display is hidden "behind" the pieces and parked above or to the left of your other displays so it does not get in the way.
4. A cursor wall keeps the mouse from falling into the hidden real display, and a window rescue moves any window that ends up there onto a piece.
5. On stop (or exit, or crash recovery) the original layout is restored.

## Requirements

- Windows 10 21H2 or newer, or Windows 11, x64.
- Parsec Virtual Display Driver 0.45 (see below).

### Install the driver

The driver is Parsec's proprietary software. It is **not bundled** with this app. Install it in one of two ways:

- In the app: **Settings -> Driver -> Download & install driver**. It downloads the official installer, verifies the Authenticode signature (must be "Parsec Cloud, Inc."), then starts it (UAC prompt).
- Manually: download <https://builds.parsec.app/vdd/parsec-vdd-0.45.0.0.exe> and run it. Project page: <https://github.com/nomi-san/parsec-vdd>.

## Install the app

- Download `virtual-desktop-display-vX.Y.Z-windows-x64.zip` from [GitHub Releases](https://github.com/Tihi321/virtual-desktop-display/releases), unzip it anywhere and run `virtual-desktop-display.exe`.
- Or build from source: `cargo build --release` (the exe is in `target\release`).

## Quick start

1. Run the exe. On first run the Settings window opens (afterwards: tray icon, double-click or **Settings...**).
2. **Splits** tab -> **Add split**.
3. Pick the **Display** from the dropdown (detected displays are listed with size, and "primary").
4. Choose a **Layout**: Columns, Rows, Grid or Custom, and the counts. The preview shows the pieces; the table lists each piece's size and whether it is "built-in" or needs a "custom slot".
5. If a piece needs a custom resolution, open the **Driver** tab and click **Register custom resolutions (admin)** once.
6. **Save & apply**, then **Start all** (or let it start on launch).

Other options per split: Enabled, Auto-split on connect, piece order (rotate which part each virtual display shows), Main piece (only for the primary display) and Refresh rate. The footer shows the current status. Other tabs: **Hotkeys** and **General** (Start with Windows, Start splitting when the app launches).

## Custom resolutions

Parsec VDD has a built-in list of modes (1920x1080, 2560x1440, 3440x1440, 1280x720, ...). Piece sizes that are not on that list, such as 1720x1440, need a **custom slot**.

- At most **5** custom slots exist in total (stored under `HKLM\SOFTWARE\Parsec\vdd`).
- Writing them needs administrator rights, so the app asks once through UAC (**Register custom resolutions (admin)**). No driver restart is needed; the modes apply the next time the splits start.
- This **replaces** any custom modes Parsec already had registered.
- If more than 5 distinct custom sizes are needed, the app falls back to the nearest larger built-in mode and stretches the picture.

## Tray menu

| Item | Action |
| --- | --- |
| Settings... | Open the settings window (also double-click the icon) |
| One checkable item per split | Enable or disable that split (restarts when running); shows "(not connected)" when missing |
| Start all / Stop all | Start or stop all enabled splits |
| Cycle pieces | Rotate the pieces of all running splits |
| Start with Windows | Toggle startup for the current user |
| Exit | Stop and restore the layout, then quit |

## Hotkeys

| Action | Default | Enabled by default |
| --- | --- | --- |
| Toggle splits (start / stop) | `Ctrl+Alt+S` | yes |
| Cycle pieces | `Ctrl+Alt+W` | yes |
| Open settings | `Ctrl+Alt+D` | no |

Change or disable them in **Settings -> Hotkeys**: use the master switch "Enable global hotkeys", the per-action Enabled box, and click the key field then press the new combination. A combination needs a modifier (Ctrl, Alt, Shift, Win) unless it is an F-key. After **Save & apply** the status shows registered or "in use by another app" when another program already owns it.

## Auto-split on connect

A split with **Auto-split on connect** starts by itself when its display is connected (and stops cleanly when it is unplugged). If you stop a split manually (Stop all, hotkey), it will not restart until that display is unplugged and plugged in again.

## Splitting the main display

You can split the primary display too. The **Main piece** (marked with a star in the preview) becomes the new primary display and carries the taskbar and desktop icons. The other pieces are placed next to it.

## Configuration

Files live in `%APPDATA%\virtual-desktop-display\`:

- `config.toml`: the settings (edited by the Settings window; manual edits need **Save & apply** or an app restart).
- `status.json`: written by the tray app (running splits, errors, hotkey registration results, driver state).
- `logs\`: daily rolling `vdd.log` files.

A v0.1 config (`physical_match`, `swap`) is migrated automatically on load.

```toml
version = 2
autostart = false          # start with Windows
start_on_launch = true     # start splits when the app launches

[hotkeys]
enabled = true             # master switch
toggle_all    = { keys = "Ctrl+Alt+S", enabled = true }
cycle_order   = { keys = "Ctrl+Alt+W", enabled = true }
open_settings = { keys = "Ctrl+Alt+D", enabled = false }

[[splits]]
name = "FlipGo"
enabled = true
auto_on_connect = true
display_id = "RTK4707"     # EDID manufacturer + product, stable across reboots
name_match = "FlipGo"      # optional fallback: friendly-name substring
layout = "rows"            # columns | rows | grid | custom
columns = 1
rows = 2
ratios_x = []              # custom only: fractions summing to 1
ratios_y = []
order = [0, 1]             # which cell each virtual display shows
main_piece = 0             # used only when the display is the primary
refresh_hz = 60
```

## Command line

| Flag | Meaning |
| --- | --- |
| (none) | Tray app |
| `--settings` | Open only the settings window |
| `--cli` | Console mode: start the splits, Ctrl+C to stop |
| `--seconds N` | Like `--cli`, stops after N seconds |
| `--register-modes "1720x1440@60,..."` | Elevated helper that writes the Parsec custom modes to HKLM and exits |

## Limitations

- About one frame of latency: not suitable for competitive gaming.
- DRM-protected video (for example some streaming apps) shows black in the capture.
- No touch input on the pieces.
- At most 8 virtual displays in total, and at most 5 custom sizes.
- Starting takes a few seconds per virtual display.
- The Windows "Identify" overlay may show numbers on the wrong display.
- HDR is not supported.

## Troubleshooting

- **Displays do not appear:** check Settings -> Driver says "Installed", the split is Enabled and its display is connected (not "(not connected)"), and the total is 8 virtual displays or fewer. Check the latest log in `%APPDATA%\virtual-desktop-display\logs`.
- **Layout was not restored** (crash or kill): start the app again and choose Stop all, or Exit from the tray. If that is not enough, open Windows Settings -> System -> Display and rearrange or re-enable your monitors.
- **Hotkey "in use by another app":** another program owns that combination. Pick a different one in Settings -> Hotkeys.
- **Driver not detected:** install it from Settings -> Driver (or manually), then click **Re-check**. A reboot may be required after the first install.

## Building

```
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
```

Rust (stable, MSVC toolchain) on Windows is required.

## CI and releases

- `.github/workflows/ci.yml` builds, runs clippy and tests on every push and pull request (windows-latest).
- `.github/workflows/release.yml` runs when you push a tag `vX.Y.Z`. The tag must match the `version` in `Cargo.toml`. It publishes a GitHub Release with `virtual-desktop-display-vX.Y.Z-windows-x64.zip` (exe, README, LICENSE) and a SHA-256 file.

## License

MIT. See [LICENSE](LICENSE). The Parsec Virtual Display Driver is separate software under Parsec's own terms.
