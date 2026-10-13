# Virtual Desktop Display

[![CI](https://github.com/Tihi321/virtual-desktop-display/actions/workflows/ci.yml/badge.svg)](https://github.com/Tihi321/virtual-desktop-display/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Tihi321/virtual-desktop-display)](https://github.com/Tihi321/virtual-desktop-display/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Split one physical monitor into several real monitors on Windows.**

Pick a display, choose columns, rows, a grid or your own ratios, and each piece becomes its own monitor as far as Windows is concerned. Fullscreen fills only that piece, Maximize and Win+Arrow snap inside it, and Teams, Zoom, Discord and OBS list it as a separate screen you can share.

```
 One 3440×1440 ultrawide              What Windows sees
┌───────────────────────────┐        ┌─────────────┬─────────────┐
│                           │  ───►  │ Display 3   │ Display 4   │
│                           │        │ 1720×1440   │ 1720×1440   │
└───────────────────────────┘        └─────────────┴─────────────┘
```

## Contents

- [Is this for me?](#is-this-for-me)
- [How it works](#how-it-works)
- [Install](#install)
- [Quick start](#quick-start)
- [Examples](#examples)
- [Custom resolutions](#custom-resolutions)
- [Tray menu and hotkeys](#tray-menu-and-hotkeys)
- [Auto-split on connect](#auto-split-on-connect)
- [Splitting your main display](#splitting-your-main-display)
- [Configuration](#configuration)
- [Limitations](#limitations)
- [Troubleshooting and FAQ](#troubleshooting-and-faq)
- [Uninstall](#uninstall)
- [Building and releases](#building-and-releases)

## Is this for me?

Good fits:

- **Screen-share half of an ultrawide.** Share only the left half in a call and keep notes or chat private on the right.
- **Fullscreen video in one part.** YouTube or a player in fullscreen fills one piece while you keep working in the other.
- **Stacked or dual-panel portable monitors on one cable**, such as the JSAUX FlipGo. Over a single HDMI or DP cable Windows sees one tall screen; this turns it back into two 1920×1080 monitors.
- **A 4K TV or big monitor as a 2×2 grid** of 1080p screens.

How it compares with window-snapping tools (FancyZones, DisplayFusion zones):

| | Snapping tools | Virtual Desktop Display |
| --- | --- | --- |
| Windows snap into zones | ✅ | ✅ (Maximize, Win+Arrow) |
| Fullscreen fills only one part | ❌ fills the whole monitor | ✅ |
| Share one part as a "screen" in Teams, Zoom or OBS | ❌ | ✅ |
| Separate taskbar per part, if Windows is set to show one | ❌ | ✅ |
| Extra latency | none | about one frame |

If you only want tidy window layouts, a snapping tool is simpler. Use this when apps need to believe a part is a real monitor.

## How it works

```
 Parsec Virtual Display Driver           this app                      your real monitor
┌─────────────┐ ┌─────────────┐   capture each piece (WGC)    ┌──────────────┬──────────────┐
│ virtual #1  │ │ virtual #2  │ ───────────────────────────►  │  piece 1     │  piece 2     │
└─────────────┘ └─────────────┘   draw into one window        └──────────────┴──────────────┘
```

1. **Virtual monitors.** The app asks the Parsec Virtual Display Driver for one virtual monitor per piece, each sized exactly to its piece. Windows treats them as real monitors.
2. **Capture and draw.** Each virtual monitor is captured on the GPU with Windows.Graphics.Capture. The images are drawn into a full-screen window on your real monitor, in the right places.
3. **Hiding the real monitor.** The real monitor still exists in Windows but is covered by that window. It is parked above or to the left of your other displays, out of the way.
   - A **cursor wall** stops the mouse from wandering onto it.
   - **Window rescue** moves any window that lands there onto the nearest piece.
4. **Clean up.** Stopping or exiting removes the virtual monitors and restores your original layout. If the app crashes, the virtual monitors disappear within a second and Windows returns to its saved layout by itself. The temporary layout is never saved to Windows' display database.

Nothing is sent anywhere. The only network access is the optional driver download from `builds.parsec.app`.

## Install

### 1. Install the driver (one time)

The app needs the **Parsec Virtual Display Driver 0.45**. It's Parsec's free, signed driver. It is **not bundled** with this app, so you install it yourself:

- **From the app:** open **Settings → Driver → Download & install driver**. The app downloads the official installer, checks that its signature is "Parsec Cloud, Inc.", then starts it. Approve the UAC prompt.
- **Manually:** download <https://builds.parsec.app/vdd/parsec-vdd-0.45.0.0.exe> and run it. More information: <https://github.com/nomi-san/parsec-vdd>.

Then click **Re-check** on the Driver tab. It should say *Installed*.

### 2. Install the app

- Download `virtual-desktop-display-vX.Y.Z-windows-x64.zip` from [Releases](https://github.com/Tihi321/virtual-desktop-display/releases), unzip it anywhere and run `virtual-desktop-display.exe`. It's portable; there's no installer.
- Or build it from source: `cargo build --release`. The exe ends up in `target\release\`.

**Requirements:** Windows 10 21H2+ or Windows 11, x64, and a GPU with up-to-date drivers.

## Quick start

1. Run the app. A tray icon appears, and on the first run the **Settings** window opens. Later, open it by double-clicking the tray icon.
2. On the **Splits** tab, click **Add split**.
3. Pick the **Display** from the dropdown. Connected displays are listed with their size, and "primary" marks your main one.
4. Choose a **Layout** (Columns, Rows, Grid or Custom) and the counts. The preview draws the pieces, and the table lists each piece's size.
5. If the table says **needs custom slot**, go to **Driver → Register custom resolutions (admin)** once. See [Custom resolutions](#custom-resolutions).
6. Click **Save & apply**, then **Start all**.

Each split also has:

- **Enabled** and **Auto-split on connect** checkboxes;
- **Rotate**, which changes which virtual monitor shows which part;
- **Main piece**, only for your primary display;
- **Refresh rate**.

## Examples

| Display | Layout | Pieces | Custom slot needed? |
| --- | --- | --- | --- |
| 3440×1440 ultrawide | 2 columns | 2 × 1720×1440 | yes (1) |
| 3440×1440 ultrawide | 3 columns | 2 × 1146×1440 + 1 × 1148×1440 | yes (2) |
| 2560×1440 | 2 columns | 2 × 1280×1440 | yes (1) |
| 3840×2160 (4K) | 2×2 grid | 4 × 1920×1080 | no |
| 1920×2160 (stacked FlipGo) | 2 rows | 2 × 1920×1080 | no |
| 3840×2160 | Custom 70 / 30 columns | 2688×2160 + 1152×2160 | yes (2) |

Inner edges are rounded to even pixels, and the last piece takes the remainder.

## Custom resolutions

Parsec's driver has a built-in list of sizes: 1920×1080, 2560×1440, 3440×1440, 3840×2160, 1280×720 and more. Pieces with any other size need a **custom slot**.

- There are **5 custom slots** in total. They're stored in `HKLM\SOFTWARE\Parsec\vdd`, so writing them needs admin rights once. **Register custom resolutions (admin)** shows a UAC prompt; nothing else in the app runs as admin.
- The driver needs no restart. The new sizes apply the next time the splits start.
- Registering **replaces** any custom sizes you set up in Parsec yourself.
- If your splits need more than 5 different custom sizes, the extra ones use the nearest larger built-in size, and the picture is scaled to fit. That looks slightly softer.

## Tray menu and hotkeys

| Tray item | What it does |
| --- | --- |
| **Settings…** | Opens the settings window (or double-click the icon) |
| *one item per split* | Enables or disables that split, restarting if running. Shows "(not connected)" if its display is missing |
| **Start all / Stop all** | Starts or stops every enabled split |
| **Cycle pieces** | Rotates the pieces of every running split |
| **Start with Windows** | Runs the app at sign-in |
| **Exit** | Stops, restores your layout and quits |

| Hotkey action | Default | On by default |
| --- | --- | --- |
| Toggle splits (start/stop) | `Ctrl+Alt+S` | yes |
| Cycle pieces | `Ctrl+Alt+W` | yes |
| Open settings | `Ctrl+Alt+D` | no |

Change hotkeys in **Settings → Hotkeys**:

- There's a master switch, plus one **Enabled** box per action.
- Click **Record** and press the new combination. It needs a modifier (Ctrl, Alt, Shift or Win), except F-keys.
- After **Save & apply**, each hotkey shows *registered*, or *in use by another app* if another program already owns that combination.

## Auto-split on connect

With **Auto-split on connect** on, a split starts as soon as its display is plugged in and stops cleanly when it's unplugged. This is handy for a portable monitor you connect now and then.

If you stop a split yourself (Stop all, the hotkey or the tray), it stays stopped until that display is unplugged and plugged in again. The app won't undo a manual stop.

## Splitting your main display

You can split your primary monitor too.

- The **Main piece** (★ in the preview) becomes the new primary display, with the taskbar, desktop icons and new windows. The other pieces sit next to it.
- When you stop, your original primary comes back.

## Configuration

Everything is set in the Settings window. The files are in `%APPDATA%\virtual-desktop-display\`:

| File | Purpose |
| --- | --- |
| `config.toml` | Your settings. After editing it by hand, click **Save & apply** or restart the app |
| `status.json` | Live state written by the tray app: running splits, errors, hotkey results, driver |
| `logs\vdd.log.YYYY-MM-DD` | Daily log. Attach it when reporting a problem |

<details>
<summary>Full <code>config.toml</code> example</summary>

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
name_match = "FlipGo"      # fallback: part of the monitor's name
layout = "rows"            # columns | rows | grid | custom
columns = 1
rows = 2
ratios_x = []              # custom only: fractions that add up to 1
ratios_y = []
order = [0, 1]             # which part each virtual monitor shows
main_piece = 0             # only used when this is the primary display
refresh_hz = 60
```
</details>

Configs from v0.1 (`physical_match`, `swap`) are converted automatically.

| Command-line flag | Meaning |
| --- | --- |
| *(none)* | Tray app |
| `--settings` | Opens just the settings window |
| `--cli` | Console mode: starts the splits; Ctrl+C stops them |
| `--seconds N` | Like `--cli`, but stops after N seconds |
| `--register-modes "1720x1440@60,…"` | Internal elevated helper that writes the custom sizes, then exits |

## Limitations

- **About one frame of extra latency.** That's fine for work, video and casual games, but not for competitive gaming.
- **DRM-protected video** (some streaming apps and sites) shows black, because protected content can't be captured.
- At most **8 virtual monitors** and **5 custom sizes**.
- **Startup speed:** starting takes a few seconds per virtual monitor, because the driver adds them one at a time.
- **No HDR and no touch input** on the pieces.
- **Settings → Display → Identify** draws the hidden real monitor's number over a corner of one piece. This is cosmetic: system overlays sit above the app's window.

## Troubleshooting and FAQ

**The pieces don't appear.**
- Check that Settings → Driver says *Installed*.
- Check that the split is **Enabled**, that its display isn't marked *(not connected)*, and that you have 8 or fewer pieces in total.
- If none of that helps, look at the latest log in `%APPDATA%\virtual-desktop-display\logs`.

**My monitors are in the wrong place after a crash.**
Start the app and choose **Exit** from the tray. If they're still wrong, rearrange them in Windows Settings → System → Display. The app never saves its temporary layout permanently.

**A hotkey says "in use by another app".**
Another program already owns that combination. Record a different one.

**The picture on a piece looks soft.**
That piece is using a fallback size because the 5 custom slots ran out, or because the custom sizes weren't registered. Open the Driver tab and register them.

**Can I use this without Parsec?**
Not at the moment. The Parsec driver is what creates the virtual monitors, and it removes them automatically if the app stops.

**Will games run fullscreen on a piece?**
Yes. Borderless and fullscreen games treat the piece as their monitor. Expect about one frame of extra latency.

## Uninstall

1. Tray → **Exit**. This restores your layout.
2. If you turned on **Start with Windows**, turn it off first, or remove `VirtualDesktopDisplay` from Startup apps.
3. Delete the app folder and `%APPDATA%\virtual-desktop-display\`.
4. *(Optional)* Uninstall **Parsec Virtual Display Driver** from Windows Settings → Apps.

## Building and releases

```
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
```

You need stable Rust with the MSVC toolchain, on Windows.

- **CI:** `.github/workflows/ci.yml` runs the format check, clippy, tests and a release build on every push and pull request.
- **Releases:** push a tag `vX.Y.Z` that matches `version` in `Cargo.toml`. `.github/workflows/release.yml` then publishes `virtual-desktop-display-vX.Y.Z-windows-x64.zip` (exe, README, LICENSE) and a SHA-256 checksum.

## License

MIT. See [LICENSE](LICENSE). The Parsec Virtual Display Driver is separate software under Parsec's own terms.
