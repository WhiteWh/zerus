# Desktop presence sampler

`zerus-presence-sampler` reads input idle time and screen lock state. It does not
publish presence, register services, or run inside Zerus Next. The desktop
service integration is a separate change.

Keep one `Sampler` and call `sample()` from a background thread at least once
every five seconds. Construction is lazy. `Sample.idle` and `Sample.locked` are
independently optional: unavailable data must not become zero idle or unlocked.
Before a writer interprets a short idle duration as presence it must also require
`Idle.since_input`. An observation-only lower bound can establish absence once
large enough, but a small value cannot establish recent input.
Linux method replies time out after 500 ms; IPC setup and platform APIs are not
hard real-time operations.

* KDE Wayland uses `ext_idle_notifier_v1` version 2 input notifications, which
  ignore video/power idle inhibitors. The result is explicitly a **lower bound**,
  not evidence of recent input. Startup is unknown until the first idle event;
  continued input stays active, and repeated samples never renew an idle event.
  Every seat must be known, and the least idle seat wins. Compositor liveness is
  checked separately without changing activity. A disconnect, stale roundtrip,
  or polling gap discards the old observation.
* GNOME uses Mutter's millisecond `GetIdletime`. KDE X11 uses its millisecond
  ScreenSaver method. KDE's method and Xwayland are never Wayland fallbacks.
  Lock state uses the KDE/GNOME ScreenSaver interfaces without service activation.
* macOS uses the public Quartz combined-session input idle API and current
  console-session dictionary, matching the previous Qt sources. Lock and fast
  user switching are distinguished; missing/invalid session data stays unknown.
  It does not install an event tap or read keyboard/mouse event contents.
* Windows 10/11 uses `GetLastInputInfo` in this process's active interactive
  session and `WTSSessionInfoEx` for its lock state. Session 0, disconnected
  sessions and failed queries stay unknown. After the 32-bit input counter can
  have wrapped, an initial age is only a lower bound until a new input timestamp
  is observed. Regressing input timestamps do not establish recent presence.
  It does not install an input hook, retain user names or read input contents.
* Other platforms return unknown until their platform backend is implemented.

Tests use private D-Bus peer connections, an in-process Wayland compositor, and
platform value/state fixtures. They do not sample the developer's desktop or
write the existing `desktop-presence.json` file.

```sh
cargo test --locked --manifest-path crates/presence-sampler/Cargo.toml
```
