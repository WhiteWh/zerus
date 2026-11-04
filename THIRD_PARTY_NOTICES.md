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

`tray/vendor/libvterm` contains libvterm 0.3.3 under MIT. Its original copyright
notice and complete license are retained in `tray/vendor/libvterm/LICENSE`;
source provenance, archive checksum and one local patch are in `UPSTREAM.md`
beside it.

`tray/vendor/md4c` contains the md4c 0.5.2 Markdown parser and entity table
under MIT. Its original license is retained in `tray/vendor/md4c/LICENSE.md`;
source provenance and archive checksum are in `UPSTREAM.md` beside it.

`tray/vendor/qrcodegen` contains the Nayuki QR Code generator 1.8.0 (C++) under
MIT, which draws the phone pairing code. Its license is retained in
`tray/vendor/qrcodegen/LICENSE`; source provenance and file checksums are in
`UPSTREAM.md` beside it.

## GitHub logo

The About page uses the [GitHub mark from Primer Octicons](
https://github.com/primer/octicons/blob/97825f832c98f817867f770d084c08e3edc6f78c/icons/mark-github-16.svg). Its complete MIT license and GitHub copyright notice are retained
in `tray/resources/icons/github-mark.svg`, including the embedded Qt resource.

## Agent logos (WhiteWh fork)

New session and the team views show the Codex, Claude, Kimi, DeepSeek, OpenCode and Ollama logos from
[LobeHub lobe-icons](https://github.com/lobehub/lobe-icons), MIT, Copyright (c) 2023
LobeHub. Its license is retained in `tray/resources/agent-icons/LICENSE`, also in
the embedded Qt resource; the Ollama logo is recolored to `#e6e8eb`, and the team views
paint the monochrome parts of the Kimi and OpenCode logos in the theme's text color.

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

The legacy Qt GUI links Qt 6 (Widgets, Network and Multimedia), and on Linux the KDE
Frameworks KStatusNotifierItem, KWindowSystem and KIdleTime. These retain their
own licenses. On Linux, the optional embedded DeepSeek native UI plugin
(`libhgs-tray-native-ui.so`) links Qt WebEngine, which includes Chromium and
additional third-party components. See the
[Qt WebEngine licensing documentation](https://doc.qt.io/qt-6.8/qtwebengine-licensing.html)
and the notices supplied by the exact Qt distribution used to build the application.

The Arch package ships Zerus Next and dynamically links system GTK3,
WebKitGTK 4.1, JavaScriptCoreGTK, ALSA and GStreamer. It does not bundle those
libraries, Qt/KDE or Chromium; their system distributions supply notices and
corresponding source. The Linux Qt source-build fallback is kept separately
until Linux acceptance and is not part of the Arch package.
`scripts/collect-licenses.py` includes the host-target Cargo notices for `hgs`,
the connector, and the client with its media/video features, together with
Zerus MIT and the Codex data notice. Its client/media and client/core locks
supplement `client/src-tauri/Cargo.lock`. A missing dependency notice fails
packaging. Review source/relinking obligations when linkage or bundled
components change. MIT licensing of Zerus does not relicense its dependencies.

The Windows portable zip is different: it bundles Qt 6 for MSVC, including Qt
Multimedia with its audio runtime, next to the application. It contains no Qt
WebEngine or Chromium. These are separate, replaceable DLLs. Qt is used under the GNU LGPL
version 3; its third-party components retain their own licenses, and the zip's
`README.txt` names the exact Qt source archive and Git tag. The zip carries
the SPDX documents supplied
with the exact Qt installation, which name the license and copyright of each Qt
module and of the third-party code inside it (libjpeg-turbo, FreeType, HarfBuzz,
libpng, zlib and others), and `packaging/windows/licenses` with the text of every
standard license and exception those documents name. Based in part on the work of the Independent JPEG Group
and of the FreeType Team. The zip also carries the Rust, libvterm, md4c and Codex
notices and the Microsoft Visual C++ runtime DLLs, redistributed with the
application from Visual Studio's redistributable folder. Next to `hgs.exe` it
carries Microsoft's ConPTY, the unmodified x64 `conpty.dll` and `OpenConsole.exe`
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
compiler and no software OpenGL fallback. `scripts/package-windows.py` fails when
the Qt documents are missing or name a license without a shipped text, and when
the bundle contains one of those libraries.

The macOS application distributed through the Homebrew cask is different: it
bundles the official Qt 6.11.2 frameworks and plugins it needs (including Qt SVG
and Qt Multimedia) as unmodified, dynamically linked and replaceable Qt components
under LGPL-3.0-only. It contains no Qt WebEngine or Chromium. Third-party audio components retain their own license notices.
`Zerus.app/Contents/Resources/licenses/qt` contains the LGPL-3.0 and GPL-3.0
texts, Qt's official SPDX documents for every bundled module, including its
third-party components, and the location of the exact Qt sources. It also
includes the standard license and exception texts those documents name;
custom LicenseRef texts remain in the documents. This conservative coverage
includes alternatives and optional components in the module metadata, without
asserting that each component is compiled. The parent `licenses` folder carries
the Zerus, libvterm, md4c, Codex and Cargo dependency notices.
`scripts/check-macos-bundle.py` rejects missing notices and Qt identifiers without
their texts.

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
