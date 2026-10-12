<img src="assets/icon.svg" width="64" height="64" alt="">

# Databaser

A small Windows desktop app for starting, stopping and restarting your local
**MySQL**, **PostgreSQL** and **SQL Server** services — and for deciding how
they start — without opening Task Manager or Services.

Built in Rust with [GPUI Kit](https://gpui-kit.com) (`gpui-kit` 0.7.1).

## What it does

- Scans the Windows Service Control Manager on launch and lists every local
  MySQL/MariaDB, PostgreSQL and Microsoft SQL Server service (matched by service
  name *and* the executable behind it, e.g. `mysqld.exe` / `pg_ctl.exe` /
  `sqlservr.exe`). For SQL Server only the database engine is listed; the Agent,
  Browser, CEIP and VSS Writer helpers are not.
- Shows each service with a live status indicator, refreshed every 2 seconds,
  so it stays correct even if something else changes the service.
- Per service:
  - **Stopped** → `Start`
  - **Running** → `Stop` and `Restart`
  - **Details** → a popover with the display name, SCM name, engine type,
    installed **version**, current state, **listening ports**, start-up type and
    the binary path.
  - **Start-up** → *Automatic* / *Manual* / *Disabled*. Manual means "do not
    start at boot, but I can start it"; Disabled means Windows will not start it
    at all. Only the start type changes — nothing else about the service is
    touched.
  - While an action runs the buttons are replaced by a spinner.
- A single **Menu** holds everything else:
  - **Rescan**
  - **Start Databaser with Windows** (off by default)
  - **Run in background (minimize to tray)** (off by default)
  - **Theme** → *System* (follows Windows) / *Light* / *Dark*, remembered between
    launches and ticking the active choice
  - **Advanced** → *Enable no-prompt control* / *Remove elevated control*
  - **Check for updates**, **About Databaser**

Starting a service that is disabled says so and points at the fix, rather than
passing on the Service Control Manager's opaque "cannot accept the request in
its current state".

## Background and sign-in

Both are opt-in and stored per user.

- **Run in background** keeps the tray icon present and makes closing the window
  *hide* Databaser instead of exiting it: it keeps polling, and the tray icon
  (or its **Open Databaser** item) brings it back. Its **Quit** item exits for
  real. With the option off, closing the window exits as usual.
- **Start Databaser with Windows** writes a single per-user `Run` entry:

  ```
  HKCU\Software\Microsoft\Windows\CurrentVersion\Run
    Databaser = "C:\…\databaser.exe" --startup
  ```

  That needs no administrator rights, so the toggle never prompts. The registry
  is the source of truth: the menu reads it back, so turning the entry off from
  Task Manager → Startup apps is reflected in the app. With background mode also
  on, `--startup` makes the app come up hidden in the tray rather than opening a
  window.

Only one copy of Databaser runs at a time. A second launch brings the existing
window to the front and exits, which matters once starting with Windows is on.

## Install

Download from the [latest release](https://github.com/AbrahamX3/databaser/releases/latest):

- **`Databaser-Setup-<version>.exe`** — recommended. A per-user installer (no
  UAC) that adds a Start Menu shortcut and an uninstall entry. It installs to
  `%LOCALAPPDATA%\Programs\Databaser`, which the in-app updater can write to.
- **`databaser-x86_64-pc-windows-msvc.zip`** — portable. Unzip and run
  `databaser.exe` anywhere.

Either way, updating is handled in the app: **Menu → Check for updates**.

## Running it

```powershell
cargo run
```

The first build compiles a lot of UI dependencies and takes several minutes.
Later builds are fast.

### Requirements

- Rust (MSVC toolchain) 1.92 or newer — this project is built on 1.99.
- Visual Studio Build Tools with the **Desktop development with C++** workload.
- CMake available to the `cmake` crate, either on `PATH` or via a `CMAKE`
  environment variable pointing at `cmake.exe`.

## How the elevation works

Starting or stopping a Windows service needs administrator rights, but the GUI
itself should not run elevated. Databaser picks the best available option:

1. **Already elevated** → the action runs in-process, no prompt.
2. **No-prompt control enabled** (see below) → the action is handed to a
   scheduled task, no prompt.
3. **Otherwise** → the per-action fallback: a short-lived elevated copy of this
   executable runs `--ctl <action> <service> --out <file>`, which prompts for
   UAC once per click.

### Enabling no-prompt control (one-time setup)

Click **Enable no-prompt control** in the header. That asks for UAC **once** and
registers a Windows Scheduled Task named `DatabaserElevatedControl`, marked
*Run with highest privileges*, whose action is `databaser.exe --ctl-from-task`.
The task has no trigger, so it only ever runs when Databaser asks it to.

After that the GUI stays unelevated: each Start/Stop writes a request file to
`%LOCALAPPDATA%\Databaser\ctl-request.json`, runs the task, and reads the
helper's reply from `ctl-response.json`. No more prompts.

Which mode is in force is reflected in **Advanced**: *Enable no-prompt control*
is only offered while the per-action prompt is in use, and *Remove elevated
control* only while the task exists. If the executable is moved or rebuilt the
task is detected as stale and the setup item comes back. Removing the task (Task
Scheduler, or `schtasks /delete /tn DatabaserElevatedControl`) reverts to the
per-action prompt.

Changing a service's **start-up type** needs the same rights as starting it, so
it travels the same three paths — including the no-prompt task, which is another
reason to enable it.

## Project layout

```
src/
  main.rs        # entry point; dispatches GUI vs. helper modes, single instance
  app.rs         # the GPUI window: header + menu, cards, popovers, About dialog
  assets.rs      # serves the embedded brand icon over GPUI Kit's defaults
  autostart.rs   # the "start with Windows" Run entry
  tray.rs        # the tray icon and its menu (background mode)
  platform.rs    # the Win32 calls with no safe wrapper: mutex, show/hide window
  elevation.rs   # control-mode selection, per-action UAC fallback
  helper_task.rs # no-prompt control: the scheduled-task setup + client
  ctl.rs         # privileged helper entry points (--ctl / --ctl-from-task)
  settings.rs    # theme, background mode, update cache (%APPDATA%\Databaser)
  services/
    model.rs     # ServiceInfo / ServiceKind / ServiceState / ServiceAction
    error.rs     # ServiceError (thiserror) for discovery + control
    scm.rs       # discovery, status queries, start type (read-only, unelevated)
    version.rs   # installed versions, from the registry or the data directory
    ports.rs     # listening ports, from the kernel socket table
    control.rs   # start / stop / restart / start type (runs elevated)
build.rs         # Windows exe metadata + icon
scripts/         # release notes, tray icon generation
```

## Icon

`assets/icon.svg` is the app's icon artwork; it is also rendered in the About
dialog. `assets/icon.ico` is the same artwork in Windows icon format, embedded
into the executable by `build.rs` — that is what Windows shows in the **title
bar, taskbar and Alt-Tab**.

`assets/tray-32.rgba` is the same artwork as raw 32×32 RGBA, which is what the
tray backend takes. The app ships no image decoder, so it is baked once by
`scripts/make-tray-icon.ps1` and committed. Re-run that script only if
`assets/icon.ico` changes:

```powershell
powershell -File scripts/make-tray-icon.ps1
```

## Releases and updates

Releases are built by CI. Pushing a tag triggers
`.github/workflows/release.yml`, which builds the release binary on
`windows-latest` and publishes two assets to the GitHub Release:

- `databaser-x86_64-pc-windows-msvc.zip` — contains `databaser.exe`
- `SHA256SUMS` — the archive's SHA-256

The asset name includes the target triple because the in-app updater
(`self_update`) matches release assets by target.

To cut a release, bump `version` in `Cargo.toml` and push a matching tag:

```powershell
# edit Cargo.toml: version = "0.2.0"
cargo build --release    # refreshes Cargo.lock
git commit -am "chore: release v0.2.0"
git tag v0.2.0
git push && git push --tags
```

In the app, **Menu → Check for updates** asks GitHub Releases whether a newer
version exists (verified against `SHA256SUMS` before installing). Installing
replaces the executable; the running process keeps the old binary until it is
restarted, so the app then offers **Restart**.

Databaser also checks **automatically a few seconds after launch, at most once a
day** (the last check is cached in `settings.json`). The automatic check is
silent: it reports nothing when there is no update and ignores network errors —
only the manual menu action surfaces those. Dismissing a banner with the **×**
remembers that version, so launch checks will not raise it again (a newer
version still will, and a manual check always shows what it finds).

### Commit messages

Commits follow [Conventional Commits](https://www.conventionalcommits.org/):
a **short imperative subject**, with the detail in the **body**.

```
feat: check for updates at launch

Runs a few seconds after start, at most once per day, cached in
settings.last_update_check. Silent when up to date or on a network
failure; only the manual menu action reports those. Adds a dismiss button
that records skipped_version, so launch checks stop raising it.
```

The subject's type drives the release notes: `feat`, `fix`, `perf`, `docs` and
`refactor` each get their own section, and everything else lands under
"Maintenance".

### Release notes

GitHub's own generator only lists pull requests, and this project pushes
straight to `main`, so the workflow builds the notes instead:
`scripts/release-notes.ps1` groups the commits between the previous tag and the
new one and writes the release body.

## Development

Linting follows the strict settings in `[lints]` (`clippy::all` denied,
`unsafe_code` forbidden):

```powershell
cargo clippy --all-targets --all-features --locked -- -D warnings
```

- `unsafe_code` is **denied**, not forbidden: three small modules call Win32
  directly because no safe wrapper exists — `platform.rs` (single-instance
  mutex, showing and hiding the window), and `services/control.rs` and
  `services/ports.rs` (changing a start type; reading the socket table). Each
  call site carries an `#[expect(unsafe_code, reason = "…")]` explaining why,
  so an accidental `unsafe` anywhere else is still a build error.
- Errors use typed `thiserror` enums (`ServiceError`, `ElevationError`)
  rather than strings; the UI shows their `Display` text.
- Blocking Windows SCM and UAC work runs on gpui's *background* executor, so
  the UI thread never blocks, and service state polling does its SCM queries
  off the UI thread too.

## Roadmap

Everything originally planned has shipped: the listening ports and server
versions, the per-service start-up type, and the details popover they live in.
The menu already carries every setting, so no separate settings screen.

Deliberately not planned:

- **Pause / Resume**, editing dependencies, the log-on account, or
  recovery-on-failure — the database engines rarely need these, and the app is a
  tool for the database engines rather than a Task Manager replacement.
- **Automatic (Delayed Start)** — preserved when another tool sets it, but not
  offered here.
- **Global hotkeys** — the tray icon is how the app is reached when the window
  is hidden.

Code signing for the executable and installer has been evaluated and deferred:
it is a distribution concern rather than a product one, and a self-signed
certificate behaves identically to no signature as far as SmartScreen is
concerned.
