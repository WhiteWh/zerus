# Third-party components

Zerus original source and artwork are licensed under the MIT license in `LICENSE`.
Dependency licenses and copyright notices remain in effect.

## Vendored code

`src/state/data/codex-0.160.1-greetings.txt` retains the finite native empty-state
greeting vocabulary from OpenAI Codex `rust-v0.160.1` for exact reset recognition.
`src/state/data/codex-empty-state-60x21.txt` retains its released settled idle-logo
glyphs, verified against 0.160.1, 0.162.0 and 0.162.1. Source provenance is recorded
in each file; the original Apache-2.0 license is
retained in `docs/licenses/codex-LICENSE.txt`, which is also shipped with packages.
No native Codex executable is bundled.

## GitHub logo

The About page uses the [GitHub mark from Primer Octicons](
https://github.com/primer/octicons/blob/97825f832c98f817867f770d084c08e3edc6f78c/icons/mark-github-16.svg). Its complete MIT license and GitHub copyright notice are retained
in `packaging/desktop/icons/github-mark.svg`.

## Agent logos (WhiteWh fork)

New session and the team views show the Codex, Claude, Kimi, DeepSeek, OpenCode and Ollama logos from
[LobeHub lobe-icons](https://github.com/lobehub/lobe-icons), MIT, Copyright (c) 2023
LobeHub. Its license is retained in `tray/resources/agent-icons/LICENSE`, also in
the embedded Qt resource; the Ollama logo is recolored to `#e6e8eb`, and the team views
paint the monochrome parts of the Kimi and OpenCode logos in the theme's text color.
The Tauri desktop client shows the same logos: the files are copied unchanged into
`client/src/assets/agent-icons` (Codex, Claude, DeepSeek, Kimi, OpenCode and Ollama,
with the license), inlined into the built interface, and monochrome parts follow the
theme's text color. `client/scripts/collect-notices.py` ships that license in every
desktop package beside the npm dependency notices (`licenses/frontend/lobe-icons-agent-logos`).

## CanonicalPath lexical path identity

The CLI links `canonicalpath-rust` from CanonicalPath release `2026.10.24-1`,
Git revision `50b390675a146874b1555f361e491d1efeb393a7`, under MIT,
Copyright (c) 2026 Roman Ilyin. The crate still declares version `2026.5.18-2`;
`Cargo.lock` identifies the exact release source. Its root-level license is
retained in `docs/licenses/canonicalpath-LICENSE.txt`. The notice collector checks
the full source pin and license digest before including that text in packages,
because this revision's Rust package metadata does not declare its root license.

## Web client design mockup

The design mockup in `design/webui` is not shipped with Zerus packages. It
embeds the outline paths of two [Tabler Icons](https://github.com/tabler/tabler-icons)
3.50.0 glyphs (`brand-apple`, `brand-windows`) as operating system marks in
`design/webui/src/lib/os-icons.ts`, under MIT; the complete license and copyright
notice are retained in `design/webui/licenses/tabler-icons-LICENSE.txt`. Its
Linux mark is original Zerus artwork. `design/webui/src/lib/icons.generated.ts`
contains static markup generated from the installed `lucide-solid` package (ISC).

## Dependencies obtained during build

### Desync artifact delivery and standalone executor

The Windows artifact implementation in `scripts/experiments/desync-artifact` uses desync
revision `717d63b1756554abc470671ea591ad6d25e6059d`, under BSD-3-Clause,
Copyright (c) 2017, folbricht. Its unchanged license text is retained in
`docs/licenses/desync-LICENSE.txt`. The experiment applies a local patch exposing
an explicit displacement-buffer limit, adds synthetic metadata-only planner
instrumentation and uses delete-on-close for Windows null-seed scratch files;
the upstream copyright and license
remain in effect. Zerus original code remains MIT. The author's name is not
used to imply endorsement.

Offline builds collect reviewed notices for the actual linked Go modules and
runtime beside their binary. `services/artifact-executor/package.py` verifies
the binary, inventory and notice hashes and includes the complete texts in the
standalone executor package's licenses directory. It includes no SDK, runtime
configuration or proprietary payload. The WhiteWh fork's Windows zip ships the
same verified `hgs-executor.exe` beside `hgs.exe`, with those notices under
`licenses/executor` (`scripts/build-artifact-executor.py`,
`scripts/package-windows.py --executor`); a root project notice alone is not a
substitute for the linked dependency inventory.

### Rust dependencies

Rust dependencies and versions are locked in `Cargo.lock` and, for the mobile
connector that Linux packages also ship, `services/mobile/connector/Cargo.lock`.
hgs links Loro (MIT) through `crates/replica`; Loro in turn links `im`,
`bitmaps` and `sized-chunks` under MPL-2.0, unmodified from crates.io, where
their source code is available. Packages carry their MPL-2.0 license texts.
Crates that publish no license file get the upstream notices kept in
`packaging/rust-licenses`.
The license expressions and source repositories of the first are recorded in
`docs/rust-dependencies.json`; packages carry the notices of both.
This inventory is metadata, not a replacement for dependency license texts.
Cargo downloads the corresponding sources and license files from crates.io.

The desktop client in `client/` locks its Rust dependencies in
`client/src-tauri/Cargo.lock`. It links `selectors` under MPL-2.0, unmodified
from crates.io, where its source code is available; its bundles carry the notices
of every crate they ship, from the crates or from `packaging/rust-licenses`.

The Arch package ships Zerus Next and dynamically links system GTK3,
WebKitGTK 4.1, JavaScriptCoreGTK, ALSA and GStreamer. It does not bundle those
libraries, Qt/KDE or Chromium; their system distributions supply notices and
corresponding source.
`scripts/collect-licenses.py` includes the host-target Cargo notices for `hgs`,
the connector, and the client with its media/video features, together with
Zerus MIT and the Codex data notice. Its client/media and client/core locks
supplement `client/src-tauri/Cargo.lock`. A missing dependency notice fails
packaging. Review source/relinking obligations when linkage or bundled
components change. MIT licensing of Zerus does not relicense its dependencies.

The Windows portable zip ships Zerus Next and its actual Rust/frontend notices,
with the Microsoft Visual C++ runtime DLLs from Visual Studio's redistributable
folder. WebView2 uses the separately installed Evergreen runtime. It deploys no
Qt runtime. Next to `hgs.exe` it carries Microsoft's ConPTY, the unmodified x64 `conpty.dll` and `OpenConsole.exe`
from the NuGet package `Microsoft.Windows.Console.ConPTY` 1.25.260930003 (built
from [Windows Terminal](https://github.com/microsoft/terminal)), under MIT. Its
license, from that repository's `LICENSE` (tag `v1.25.2733.0`, unchanged since
the first release), is retained in `docs/licenses/conpty-LICENSE.txt` and shipped
as `licenses/conpty-LICENSE.txt`; the package declares MIT and carries no license
file. `OpenConsole.exe` also contains third-party code, such as Chromium's numerics
library under BSD-3-Clause; the repository's `NOTICE.md` from the same tag lists it
with each license and is retained in `docs/licenses/conpty-NOTICE.md` and shipped
as `licenses/conpty-NOTICE.md`. `packaging/windows/conpty.json` pins the package
and both files by SHA-256, and no other file of the package is shipped. It contains no Windows
system library, such as ICU or the Direct3D shader compiler, no Windows SDK shader
compiler and no software OpenGL fallback. `scripts/package-windows.py` rejects obsolete Qt runtime payloads in new packages.

The macOS application is the signed/notarized Tauri `Zerus.app`. Its
`Contents/Resources/licenses` folder carries the Zerus/Codex, Cargo and frontend
notices for its actual dependencies. No Qt frameworks or plugins are bundled.
Previously published releases remain immutable with their original notices;
read-only compatibility validators do not authorize newly packaging Qt.

The CLI's `rusqlite` build includes SQLite through `libsqlite3-sys`. The collected
notices also preserve its upstream public-domain dedication, separate from the
Rust binding's MIT notice. [SQLite copyright](https://www.sqlite.org/copyright.html).

Native agent CLIs are installed separately by the user and are not included here.

## Android Markdown dependencies

The Android application bundles CommonMark Java 0.30.0 and its GFM tables,
strikethrough, task-list and autolink extensions under BSD-2-Clause, plus
autolink-java 0.12.0 under MIT. Both projects are maintained by Robin Stocker
and contributors. Their exact upstream license texts are retained in
`mobile/android/app/src/main/assets/licenses/commonmark-java.txt` and
`mobile/android/app/src/main/assets/licenses/autolink-java.txt` and shown in the
application's third-party notices. Sources: [CommonMark Java](
https://github.com/commonmark/commonmark-java/tree/commonmark-parent-0.30.0) and
[autolink-java](https://github.com/robinst/autolink-java/tree/autolink-0.12.0).

## Android libraries

The Android application also contains AndroidX, Jetpack Compose, Kotlin,
kotlinx, OkHttp, Okio, CameraX, ZXing, Guava and other libraries, almost all
under Apache-2.0, and in the published build Firebase and Google Play services.
`scripts/android-notices.py` generates the list of every library in the
published release configuration with its license from Gradle's resolution of that
configuration, the exact artifacts it resolved and the libraries' POM files:
`mobile/android/app/src/main/assets/licenses/libraries.tsv`.
Libraries under BSD or MIT licenses point to reviewed texts with their copyright
notices in the same folder, including Checker Framework qualifiers (MIT) and the
Protocol Buffers code that DataStore repackages (BSD-3-Clause, from the
[protobuf `LICENSE`](https://github.com/protocolbuffers/protobuf/blob/v28.2/LICENSE),
unchanged across releases). `library-notices.txt` keeps the NOTICE files the
libraries ship, among them OkHttp's note that its public suffix data comes from
the Public Suffix List under MPL-2.0. Google Play services and the Firebase
analytics libraries are provided under the
[Android Software Development Kit License](https://developer.android.com/studio/terms);
`google-notices.txt` keeps, verbatim, the notices of the open source code they
embed. The application shows all of these in Third-party notices. Apache-2.0
text: `mobile/android/app/src/main/assets/licenses/Apache-2.0.txt`, from SPDX
license-list-data `v3.29.0` like the Windows texts.
