# Startup application

Most Xila devices are "just" one application: a clock, a thermostat, a
dashboard. A device boots directly into that application, without the
graphical shell and its login, when its file system holds
`/configuration/system/startup.json`. Without the file, the device starts
exactly as before. This is a runtime setting, not a separate firmware build:
any installed application can be the startup application.

The Espressif firmware (`firmware/espressif`), the native runtime
(`examples/native`, graphical interface) and the browser runtime
(`examples/wasm`) read it at boot.

## The file

```json
{
  "shortcut": "dev.xila.clock",
  "user": "administrator"
}
```

- `shortcut`: the identifier of an installed shortcut. The firmware reads the
  bundle's own `/applications/bundles/<identifier>/shortcut.json` (its
  `@bundle/` paths resolved, as the graphical shell does), or else a native
  application shortcut, `/configuration/shared/applications/shortcuts/<identifier>.json`
  (`settings`, `terminal`, ...). The application runs with the shortcut's
  command and arguments.
- `command` and `arguments`, in place of `shortcut`: an absolute executable
  path and its arguments, for a program that has no shortcut.
  `{"command": "/applications/executables/wasm", "arguments": ["/data/shared/kiosk.wasm"], "user": "kiosk"}`.
- `user`: the account the application runs as. It must exist. There is no
  fallback to root: write `"user": "root"` explicitly to run as root, which is
  rarely a good idea. Seeded images create the `administrator` account at
  first boot.

Other keys, such as `$schema`, are ignored.

## Behaviour

- The application replaces the graphical session. It runs without a login, on
  the screen when the board has one, with no terminal
  (`/devices/standard_io/null`). Its session task takes the user's identity
  before the application starts, as the shells do after a login: the user's
  home (created if needed) in `HOME`, the user name in `User`, the user and
  its primary group.
- On the Espressif firmware, a board without a graphical session still
  launches the configured application when `startup.json` exists and is valid,
  on the null terminal (`/devices/standard_io/null`), with no shell fallback
  if it cannot start.
- When the application ends, it starts again: at once after a run of 10
  seconds or more, otherwise after 1, 2, 4 ... seconds, up to a minute, so an
  application that fails at start does not keep the processor busy.
- The other sessions keep running: on the Espressif boards, the serial console
  stays available for maintenance.
- Problems are logged at boot and the graphical session starts instead, so the
  device stays usable: invalid JSON, an unknown shortcut or user, a missing
  program or bundle file, or a user that may not run the program. The
  graphical session also starts if the application cannot be executed at all.
  On a board without a graphical session, no shell takes the application's
  place: only the board's usual sessions start.
- Only root may write the file: it must belong to root, others may not write
  it, and its group may only if that group is root's. Otherwise it is ignored
  (with an error in the log), since it chooses the account the device runs as
  without a password. `/configuration/system` is root's.

A window fills its parent, so without the desk an application window fills
the whole screen: there is no status bar, dock or window title. On a touch
display, Core's on-screen keyboard opens when one of its text fields is tapped.

## Setting it up

- **Image**: the `wt32-sc01-plus-clock` seed profile builds a WT32-SC01 Plus
  storage image that boots into the Clock
  (`XILA_IMAGE_PROFILE=wt32-sc01-plus-clock mise run image:wt32-sc01-plus`).
  See [`tools/fs_image`](../../tools/fs_image/README.md).
- **On a device**: write the file with root's rights from the serial console,
  then restart. Remove it the same way and restart to return to the graphical
  shell; the console keeps running in both modes.

## Limitations

- **Memory.** The application saves the graphical shell's running state: its
  login screen, desk, dock, status bar and task. The graphical shell itself
  stays mounted: the mount is one small in-memory file entry and the code is
  in flash either way, and it must be there for the fallback.
- The firmware still contains the graphical shell: removing it from a build
  would be a separate, compile-time option.
