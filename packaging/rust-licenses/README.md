# Rust license notices for crates that publish none

`scripts/collect-licenses.py` takes a crate's notices from its crates.io
package. These crates ship no license file there, so their notices come from
the upstream repository instead, copied unchanged at the commit below. Before
upgrading one of them, compare the new upstream file and update the copy, the
URL and the checksum.

| Crate | Source | SHA-256 |
|---|---|---|
| `alloc-stdlib` | https://github.com/dropbox/rust-alloc-no-stdlib/blob/0a81fd6928ea3b33c8cd484aa4575d50ffb98012/LICENSE | `c0c56f26d9c051cac4d200c34c84e7ae9aaa853e01a982a1df08b09931e518ae` |
| `append-only-bytes` | https://github.com/zxch3n/append-only-bytes/blob/5ee04bd3e3249c6bfeb9c3d31644ada87bb90cbe/LICENSE | `a9ec532f4f11817f427e4f3ca0432b176138721e0866f152864b17ebdeac6bbf` |
| `arref` | https://github.com/zxch3n/arref/blob/c09551ed17509719e8f9a46d2814c4378f8786fa/LICENSE | `a9ec532f4f11817f427e4f3ca0432b176138721e0866f152864b17ebdeac6bbf` |
| `block2` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `block2` | https://github.com/madsmtm/objc2/blob/b4167b582b2f75f9a1be75495c41b765344fd03c/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `clipboard-win` | https://github.com/DoumanAsh/clipboard-win/blob/ed27008ae5d1f10cfbb266de039f0fb35fd05a56/LICENSE | `c9bff75738922193e67fa726fa225535870d2aa1059f91452c411736284ad566` |
| `dasp_sample` | https://github.com/RustAudio/dasp/blob/97c3bb9b2363c0b46ac1633858bf1054fd02a980/LICENSE-MIT | `b1d6df41ed3aa96806e74c729444d7c121d90e6660a6aed01d298e03fde475a0` |
| `dasp_sample` | https://github.com/RustAudio/dasp/blob/97c3bb9b2363c0b46ac1633858bf1054fd02a980/LICENSE-APACHE | `756c8d2ab2dc24f256e1455b5f03937f4933d83d7bc9f2d55009fc962377d512` |
| `dispatch2` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `dispatch2` | https://github.com/madsmtm/objc2/blob/8852b424193ca41602281b3d7540d7c8ed51e49a/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `dlopen2` | https://github.com/OpenByteDev/dlopen2/blob/cc80e4a0a90d499b677fdf7743699b4b3a43a989/LICENSE | `39fa265207450e77c62e90c5594a06c085b655d8374c7ced4bf7894b6bd95dd2` |
| `dlopen2_derive` | https://github.com/OpenByteDev/dlopen2/blob/cc80e4a0a90d499b677fdf7743699b4b3a43a989/LICENSE | `39fa265207450e77c62e90c5594a06c085b655d8374c7ced4bf7894b6bd95dd2` |
| `generic-btree` | https://github.com/loro-dev/generic-btree/blob/0ae02fad39fbaf6b4836e1e578e3da7759e321ed/LICENSE | `732d8393fd664a8126a46f0b7a4c23c577fdfc9658e9ad1620d5354050fb9f51` |
| `libappindicator-sys` | https://github.com/tauri-apps/libappindicator-rs/blob/eafd1e3682a1247f595410266091e9684021cb6f/LICENSE-APACHE | `a60eea817514531668d7e00765731449fe14d059d3249e0bc93b36de45f759f2` |
| `libappindicator-sys` | https://github.com/tauri-apps/libappindicator-rs/blob/eafd1e3682a1247f595410266091e9684021cb6f/LICENSE-MIT | `eb227437252b2a7a9c1fc342c93ade1f3d7ce38cc6dd754f613db07d53ceff0b` |
| `objc2` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2` | https://github.com/madsmtm/objc2/blob/d7d2fa23ceaa5e6096c923b081040e5d81b3b9df/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-app-kit` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-app-kit` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-audio-toolbox` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-audio-toolbox` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-av-foundation` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-av-foundation` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-core-audio` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-core-audio` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-core-audio-types` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-core-audio-types` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-core-foundation` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-core-foundation` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-core-graphics` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-core-graphics` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-core-media` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-core-media` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-core-video` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-core-video` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-encode` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-encode` | https://github.com/madsmtm/objc2/blob/8d214f5477365ffcbcbb7de058c86ed9a518efb7/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-exception-helper` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-exception-helper` | https://github.com/madsmtm/objc2/blob/8d214f5477365ffcbcbb7de058c86ed9a518efb7/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-io-surface` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-io-surface` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-quartz-core` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-quartz-core` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-web-kit` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-web-kit` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `objc2-user-notifications` | MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `objc2-user-notifications` | https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md | `7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54` |
| `qrcodegen` | https://github.com/nayuki/QR-Code-generator/blob/720f62bddb7226106071d4728c292cb1df519ceb/Readme.markdown#license | `5268a84dde283b89e0b810f958a6cc7a04adc6f5a435905d45a9d808685b5db3` |
| `selectors` | https://github.com/spdx/license-list-data/blob/v3.29.0/text/MPL-2.0.txt (see below) | `66a3107d5ad6a058aab753eaac2047ccb2ed0e39465dd0fe5844da3e300d5172` |
| `serde_columnar` | https://github.com/loro-dev/columnar/blob/06663cfc569c6c770b28688f015f8a87fc04e156/LICENSE | `a9af0e3a3ded007f9914a1125b736ec526d469105c6577e367e2a9ca4de39d6b` |
| `serde_columnar_derive` | https://github.com/loro-dev/columnar/blob/06663cfc569c6c770b28688f015f8a87fc04e156/LICENSE | `a9af0e3a3ded007f9914a1125b736ec526d469105c6577e367e2a9ca4de39d6b` |
| `tauri-plugin` | https://github.com/tauri-apps/tauri/blob/59b63ab500da9859f12d8888c3298bc29bba0b92/LICENSE-APACHE-2.0 | `0d542e0c8804e39aa7f37eb00da5a762149dc682d7829451287e11b938e94594` |
| `tauri-plugin` | https://github.com/tauri-apps/tauri/blob/59b63ab500da9859f12d8888c3298bc29bba0b92/LICENSE-MIT | `9dd42ea92cff2ede5cd477cbfcce051b2d0115c0ac7f368ee88cb545055dff1d` |
| `unicode-casefold` | https://github.com/spdx/license-list-data/blob/v3.29.0/text/MIT.txt (see below) | `b05785f9f18e6716bab63424b11454513b9943a222595b70411009202fc592b5` |
| `unicode-casefold` | https://github.com/spdx/license-list-data/blob/v3.29.0/text/Unicode-DFS-2016.txt (see below) | `1a33fc12a3abb0b3eb14db70e36ef1d0af51c93b1cc1f80f022c3bd406e9b874` |
| `uniffi` | https://github.com/mozilla/uniffi-rs/blob/7bc621ee9d0d15371d792612707ba5ed4111f950/LICENSE | `1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5` |
| `uniffi_bindgen` | https://github.com/mozilla/uniffi-rs/blob/7bc621ee9d0d15371d792612707ba5ed4111f950/LICENSE | `1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5` |
| `uniffi_core` | https://github.com/mozilla/uniffi-rs/blob/7bc621ee9d0d15371d792612707ba5ed4111f950/LICENSE | `1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5` |
| `uniffi_internal_macros` | https://github.com/mozilla/uniffi-rs/blob/7bc621ee9d0d15371d792612707ba5ed4111f950/LICENSE | `1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5` |
| `uniffi_macros` | https://github.com/mozilla/uniffi-rs/blob/7bc621ee9d0d15371d792612707ba5ed4111f950/LICENSE | `1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5` |
| `uniffi_meta` | https://github.com/mozilla/uniffi-rs/blob/7bc621ee9d0d15371d792612707ba5ed4111f950/LICENSE | `1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5` |
| `uniffi_pipeline` | https://github.com/mozilla/uniffi-rs/blob/7bc621ee9d0d15371d792612707ba5ed4111f950/LICENSE | `1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5` |
| `uniffi_udl` | https://github.com/mozilla/uniffi-rs/blob/7bc621ee9d0d15371d792612707ba5ed4111f950/LICENSE | `1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5` |
| `webview2-com` | https://github.com/wravery/webview2-rs/blob/edc2caf886175ccaebe86078c9cfe1ae2a187328/LICENSE | `0dcf41516e608bbcb6cdc5229feb7b86fe4a643b85e7df251133c93408fdac73` |
| `webview2-com-macros` | https://github.com/wravery/webview2-rs/blob/dffa41a8a46d3f5565eefbff2de57d38d399f158/LICENSE | `0dcf41516e608bbcb6cdc5229feb7b86fe4a643b85e7df251133c93408fdac73` |
| `webview2-com-sys` | https://github.com/wravery/webview2-rs/blob/edc2caf886175ccaebe86078c9cfe1ae2a187328/LICENSE | `0dcf41516e608bbcb6cdc5229feb7b86fe4a643b85e7df251133c93408fdac73` |

`serde_columnar` and `serde_columnar_derive` are offered under "MIT OR
Apache-2.0"; Zerus uses them under MIT, the license their repository carries.

The `uniffi` crates (UniFFI 0.32.2, tag `v0.32.2`) are under MPL-2.0; the Android
app's task replica links them (`crates/replica-ffi`). `uniffi_bindgen` and
`uniffi_udl` are built only with that crate's `bindgen` feature, for the binding
generator: the app's library never contains them, so its notices list them only
when a collection includes the generator.

The desktop client (`client/src-tauri`) adds `alloc-stdlib`, `clipboard-win`,
`dlopen2`, `dlopen2_derive`, `libappindicator-sys`, `selectors`, `tauri-plugin`,
the `webview2-com` crates and the objc2 crates to the table; the notices cover its
Linux, macOS and Windows builds (`client/scripts/check-licenses.sh` checks all
three).

- `selectors` (MPL-2.0, from servo/stylo at
  `572ecba2d1600e7c3d490586692a209faf703baa`) is used unmodified from
  crates.io, where its source is available. That repository carries no license
  file; its sources point to https://mozilla.org/MPL/2.0/, so the notice is the
  MPL-2.0 text from SPDX license-list-data at tag `v3.29.0`.
- The objc2 crates (`objc2`, `block2`, `objc2-encode`, `dispatch2`,
  `objc2-exception-helper` and the `objc2-*` framework crates, macOS only)
  carry the repository's `LICENSE.md` unchanged. It names the licenses
  without their texts or a copyright line: `objc2`, `block2` and
  `objc2-encode` are MIT, the others "Zlib OR Apache-2.0 OR MIT", which Zerus
  uses under MIT. `MIT.txt` is the MIT text from SPDX license-list-data at tag
  `v3.29.0`, unchanged, placeholder line included; the crates name Mads
  Marquart as their author. The same `LICENSE.md` notes that the framework
  crates are derived from Apple SDKs under the Xcode license.
- `libappindicator-sys` (Linux) records no repository on crates.io; its
  `.cargo_vcs_info.json` names commit `eafd1e3682a1247f595410266091e9684021cb6f`
  of https://github.com/tauri-apps/libappindicator-rs, path `sys`.
- `tauri-plugin` and `libappindicator-sys` are offered under "Apache-2.0 OR
  MIT"; both texts their repositories carry are kept.
- The commits are those in each crate's `.cargo_vcs_info.json`.

`qrcodegen` 1.8.0 ships its MIT notice only in source and documentation.
The copy is the upstream root Readme's complete License section, unchanged,
including its Markdown line-break spaces. The pinned commit is also the
crate's `.cargo_vcs_info.json` revision and the `v1.8.0` tag target. Its MIT
permission and warranty text matches Qt's `tray/vendor/qrcodegen/LICENSE`.

`dasp_sample` 0.11.0 (voice/video capture) omits the notices from its crate.
Both upstream license texts are kept at its `.cargo_vcs_info.json` revision.

`unicode-casefold` 0.2.0 (the Windows desktop shell) has no license text in its
published crate or archived upstream repository. Its published `Cargo.toml`
declares "MIT/Apache-2.0" and names Chris Wong as author. Zerus uses the MIT
option: `MIT.txt` is the same unchanged SPDX v3.29.0 text used for the objc2
crates above, including its placeholder copyright line. `NOTICE.txt` preserves
the author attribution and explains the source; no copyright year is invented.
The crate's `src/tables.rs` identifies Unicode 9.0.0, generated from the Unicode
Character Database's `CaseFolding.txt`, whose header credits Unicode, Inc. in
2016. `Unicode-DFS-2016.txt` preserves that data license's complete copyright
and permission notice from SPDX v3.29.0.
