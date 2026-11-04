# Third-party components in Zerus Next for macOS

Zerus original source and artwork use the MIT license in `LICENSE`.

The desktop GUI uses Tauri and the system WebKit, AppKit and AVFoundation
frameworks. Its Rust dependencies and their notices are recorded in
`client-rust/inventory.json` and the adjacent package directories. The bundled
hgs CLI and mobile connector carry their Rust notices in `rust/`.

The web interface includes the locked production npm dependencies listed in
`frontend/inventory.json`, including SolidJS, xterm.js, Marked and the Tauri API.
Their original license and copyright notices are included beside that inventory.
Vite's generated runtime helpers carry its notice there too. The design mockup
is not part of the shipped web interface.

The hgs native Codex reset-recognition data retains its upstream Apache-2.0
notice in `codex-LICENSE.txt`. No native agent executable is bundled.

Corresponding Zerus source and dependency locks are available at
https://github.com/ufna/zerus. Third-party license terms remain in effect.
