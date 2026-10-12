<img src="assets/icon.svg" width="64" height="64" alt="">

# Databaser

A small Windows desktop app for starting, stopping and restarting your local
**MySQL** and **PostgreSQL** services without opening Task Manager or Services.

Built in Rust with [GPUI Kit](https://gpui-kit.com) (`gpui-kit` 0.7.1).

## What it does

- Scans the Windows Service Control Manager on launch and lists every local
  MySQL/MariaDB and PostgreSQL service (matched by service name *and* the
  executable behind it, e.g. `mysqld.exe` / `pg_ctl.exe`).
- Shows each service with a live status indicator, refreshed every 2 seconds,
  so it stays correct even if something else changes the service.
- Per service:
  - **Stopped** → `Start`
  - **Running** → `Stop` and `Restart`
  - While an action runs the buttons are replaced by a spinner.
- Three-way theme switch: **System / Light / Dark**, defaulting to System and
  remembered between launches.

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

The current mode is shown next to the version in the header
(`prompt per action` / `no-prompt control` / `elevated`). If the executable is
moved or rebuilt, the task is detected as stale and the setup button returns.
Removing the task (Task Scheduler, or `schtasks /delete /tn DatabaserElevatedControl`)
reverts to the per-action prompt.

## Project layout

```
src/
  main.rs        # entry point; dispatches GUI vs. helper modes
  app.rs         # the GPUI window, state, polling and action handling
  elevation.rs   # control-mode selection, per-action UAC fallback
  helper_task.rs # no-prompt control: the scheduled-task setup + client
  ctl.rs         # privileged helper entry points (--ctl / --ctl-from-task)
  settings.rs    # theme preference, saved to %APPDATA%\Databaser\settings.json
  services/
    model.rs     # ServiceInfo / ServiceKind / ServiceState / ServiceAction
    error.rs     # ServiceError (thiserror) for discovery + control
    scm.rs       # discovery + status queries (read-only, unelevated)
    control.rs   # start / stop / restart (runs elevated)
build.rs         # Windows exe metadata + icon
```

## Development

Linting follows the strict settings in `[lints]` (`clippy::all` denied,
`unsafe_code` forbidden):

```powershell
cargo clippy --all-targets --all-features --locked -- -D warnings
```

- Errors use typed `thiserror` enums (`ServiceError`, `ElevationError`)
  rather than strings; the UI shows their `Display` text.
- Blocking Windows SCM and UAC work runs on gpui's *background* executor, so
  the UI thread never blocks, and service state polling does its SCM queries
  off the UI thread too.

## Roadmap

- [ ] Listening ports (3306 / 5432) and server versions on each card
- [ ] Start-up mode toggle (Automatic / Manual) per service
- [ ] Settings screen
- [ ] App icon and an installer
