# Native approval UI qualification

This gate covers the native frontends of the existing approval-choice implementation. It does not grant a native tool
permission, use an operator's device, install a maintained application, or qualify provider delivery. The Android runner
uses the real Compose card and `ZerusViewModel`, durable private drafts, and intercepted reserved-origin receipts. The
separate iOS fixture links the exact production `QuestionCard.swift`, `Theme.swift`, `MarkdownView.swift`, and `ZerusKit`
policies to a memory-only callback sink. It excludes the production entry point, `AppModel`, `ConversationModel`, relay
client, keychain, notifications, and storage. The desktop QTest uses the real `QuestionCard` and private draft directory.

All three native gates are required. SDK-free harness controls are admission evidence only. A build, import, setup,
timeout, cleanup, missing case, or skipped case cannot substitute for a UI assertion or count as a behavioral RED.

## Android: seven ordered cases

Use `.github/workflows/android-approval-ui.yml` on the exact independently reviewed commit. It preserves the hosted
image, Java, emulator, API35 image, foreground owned ADB server, private profile, source/tool/APK seal, actual app/test
certificate equality, AXML, defined-DEX-class, and cleanup checks from the project-picker gate. It selects the new finite
runner using `scripts/ci/android-project-ui.py --approval-choices`; arbitrary runner names are refused. The original
project-picker invocation remains the default and retains its six-case result schema.

The runner executes exactly: `countdown_once`, `broader_cancel`, `broader_confirm`, `card_replacement`,
`feedback_keyboard`, `lifecycle_no_submit`, and `readonly_stale_identity`. It requires actual physical taps, the unchanged
three-second review, a separate native confirmation, exact option/text payloads and request/run/conversation/hash
binding, four unique synthetic answer receipts, zero unexpected operations, and preservation of the unrelated composer.
Lifecycle uses the actual Activity and window, rather than an injected foreground Boolean. The stale-run case changes
the intercepted inspection, calls the ordinary production refresh, and requires the verified model to observe the
replacement run with the exact old card's run, ID and hash and both original saved drafts unchanged. A server-only
mutation is insufficient. After that proof, the existing bounded inner-disclosure scroll reveals the original option
when the refresh has reset its viewport. The same stale inspection, drafts and answer count must still be proved before
binding the owned window and fresh action nodes; the scroll neither selects an answer nor retries the negative gesture.
The stale single-choice proof accepts the original RadioButton ancestor or the fixed synthetic-role-child shape in
Compose UI 1.8.3 (BOM 2025.06.01). For that shape the exact label must belong to a unique visible, disabled,
checkable View owner with a unique direct role-only RadioButton leaf covering the same full bounds. The label's enabled
bit and the decorative radio do not prove disabled ownership. Complete child enumeration is capped at 32 for each of at
most 16 owned ancestors; unknown, foreign, ambiguous, clipped or changed geometry refuses. The first owner, witness and
label/owner geometry are retained for the final fresh proof. The Send Button admission remains unchanged. On refusal,
at most 48 fixed class/flag/geometry descriptors from the failing walk, plus one UNKNOWN marker, may reach the existing
logcat tag; logging is cooperative and cannot turn a refusal into acceptance. Source-bound memory controls exercise this
source-supported shape, not compiled Kotlin or observed native classes. The retained 4233 failure completed six cases
and four receipts before the ancestor-only option proof refused; its failing class shape remains unknown.
The helper also copies only those existing strings into bounded memory on every exit, including direct-role delegation.
The existing failure result stream appends that snapshot with a fixed completion/outcome sentinel; the pass stream stays
unchanged. At most 48 rows of 512 characters plus fixed markers fit within 25,600 characters. Helper copy faults, interruption or
truncation report UNKNOWN and preserve the original failure. Reporting does not read accessibility nodes or clocks.
The original failure serializer line stays exact: a guarded memory-only block adds the snapshot to prior failure evidence
only after constructing a bounded addition of at most 25,600 characters, including its leading newline. A prepublication fault leaves prior evidence untouched.
The snapshot describes the helper's own observations, not a later walker or unvisited children. The retained 9a4b failure
again completed six cases and four receipts, but preserved no class descriptors through its logcat-only route; its actual
class shape and helper return route remain unknown.
The retained 96fc failure completed six cases and four receipts, then recorded an owned, visible, disabled, checkable
View parent with `checked=false` and two children. No child-role facts were retained, so their role shape remains unknown. Compose
maps the parent's checked bit from its selection state independently of its role. A changed run also keys the UI's
options to the new target's draft while the stale-case oracle preserves the old target's saved answer. The fixture
therefore accepts either parent checked state only after the same complete owner/role-child proof, including fresh
revalidation. The role-only child's `!checked` requirement is unchanged. Source-bound memory controls cover the two
parent states and refuse either partial correction; they are not compiled Kotlin or native qualification.
Six fixed BEFORE/RETURNED phase markers bracket only the existing refresh-main, visible-title and verified-model waits
in `open("stale-identity")`. Other open cases emit no new markers. The original calls, predicates and order stay exact;
marker faults cannot prevent those calls. These markers use the existing 64-row phase ring and logcat channel, whose
cooperative logger and bounded tail can omit observations. A missing marker leaves the active call unknown. The retained
f556 run reached six completed cases before its original 90-second instrumentation timeout; these markers do not establish
a blocked call, reduce the production review or renew any budget.
Before its one negative physical DOWN/UP attempt, it freshly resolves the exact visible Send
label in the current owned window, requires that control to be disabled, and checks its current on-screen bounds. A
missing label, changed identity/window/geometry or enabled control fails; there is no cached-node fallback, second
gesture or further review wait. Normal enabled-action taps keep their original guard. The final evidence names this
disabled physical attempt, and the case requires no increase in answer-entry count, exactly four receipts, unchanged
drafts and a healthy interceptor afterward. This qualifies refusal of an already-observed stale inspection, rather
than requiring a network inspection on every answer. No answer is admitted in that case. The fixture cannot contact its
reserved relay: every permitted request is completed inside the interceptor, while unknown operations throw.
Android uses one card/target predicate in send admission and the shared inline/expanded card: a nonblank target run
and exact question run/conversation equality. Matching empty conversations remain valid for startup cards; no client
prefix allowlist, identity filling or draft rebinding is added. A mismatched card's option/text controls and send, skip
and approval actions become unavailable. The final case proves the same complete original answers are available on
the old target and refused on the replacement target, and freshly observes the native option and Send disabled.
An empty form after recomposition or disabled Send alone is not the binding-refusal oracle.
Case entry/completion breadcrumbs go only to the disposable emulator's `ZerusApprovalUI` log. After a failed approval
instrumentation attempt, the driver tries one five-second bounded read of the last 400 crash/main/system log records,
only while the owned emulator and ADB server remain live. This uses the original 1200-second end and 20-second cleanup
reserve. The read filters to fixture and Android-runtime tags. The interceptor atomically retains its first typed guard
failure with bounded method/encoded-path metadata only, emits that named diagnostic, and returns a fixed local failure
response without network forwarding. Every wait and final qualification refuses a retained interceptor failure.
Missing, late or failed diagnostics remain unknown and cannot replace the primary failure, qualify a missing
case, relax the strict result parser, or authorize a retry. The instrumentation cap is 240 seconds (owner direction,
2026-10-22): passing runs take about a minute, while slow standard runners took 1.3 to 1.8 times as long and exceeded
the earlier 90-second cap, once after all seven cases had completed.
Fixed lifecycle checkpoints bracket the existing body-restoration wait, confirmation-retirement wait and disabled-Send
check, outside their polling predicates. Separate markers identify return from the resumed main-thread observation,
entry into primary-failure capture and entry into cleanup. They use the same phase log, at most 64 markers plus one
UNKNOWN marker on capacity or logger failure. Logger IO is cooperative; a retained marker identifies a call region,
not a deadlock or an individual main/accessibility subread. No original read, result, assertion, input or wait is changed.
The approval instrumentation also streams at most 21 fixed scalar progress records through Android
[`Instrumentation.sendStatus`](https://developer.android.com/reference/android/app/Instrumentation#sendStatus(int,android.os.Bundle)):
entry, store/bootstrap and activity-launch boundaries, then each existing case begin/complete. Each versioned record has
only sequence, literal stage/case, completed-case count and monotonic elapsed time, is at most 160 characters, and is paired
with the exact status code. It emits nothing during a physical gesture or from a UI/input callback. The parser refuses
unknown, duplicated, reordered, oversized or truncated status framing; progress never substitutes for the unchanged
complete seven-case final result. The existing direct stdout/stderr files preserve any partial records on a timeout;
no new ADB operation, child, retry or time budget is added. The retained `b0141fce` 90-second timeout had empty output and
did not establish any reached case, review-clock or product-action failure. This streaming boundary is diagnostic only.
The fixture's [native text search](https://developer.android.com/reference/android/view/accessibility/AccessibilityNodeInfo#findAccessibilityNodeInfosByText(java.lang.String))
returns approximate matches, so candidates must refresh and retain the exact visible text/content-description check.
The original subtree traversal remains the fallback. Monotonic phase records cover card opening, physical touch and the
first disabled/zero-answer assertion; they never override the three-second policy or establish UI acceptance alone.

The three early-window assertions first use the same real Home/resume route as the lifecycle case, with the exact owned
Activity required to lose and regain focus. Their scoped Send cache retains the original verified card and owned window.
After the first review snapshot binds and current identity passes, preparation acquires the exact visible Send label once
from the resumed tree, then requires the same original window/package, a refreshed visible exact label, current identity
and the unchanged original deadline. The lifecycle case performs this one acquisition only after its existing body
restoration and confirmation-retirement checks. The private setter consumes its sole attempt before querying, including
failed queries; missing, obsolete, foreign, invisible, late or faulted preparation refuses without another acquisition.
No card, native question, selection, draft, review snapshot, generation, start or end is replaced. The later cached reader
and its entire ancestor walk remain unchanged and perform no further label lookup or fallback search.
The cache is cleared in `finally`. The monotonic timestamp before the sole resume intent is a lower bound, not the
production review-clock start. The same existing ten-second focus wait also requires a newer eligible owned production
observation; no additional wait slot is introduced. The early assertion or Enter must finish within the original three
seconds of that first actual review start. The independent lifecycle case keeps its original Home/resume boundary exactly
once, with the same cache and elapsed check around its original focus/question/confirmation-retired/count/disabled
assertions; it never performs an additional reset. No production visibility, foreground or review clock is set.

The lifecycle case starts with an open native Confirm dialog, which can remove the Activity's window focus while the
Activity is still resumed. Its single Home transition therefore waits for the same live Activity to reach lifecycle
`CREATED` with no focus before sending the existing single resume intent. It then requires that original instance to be
`RESUMED` with focus. Bounded before/stopped/resumed observations retain instance identity, lifecycle and focus. The
original ten-second wait slots and three-second early-review requirement remain unchanged. The former focus-only stop
predicate did not prove an `onStop` transition; the retained failed run does not establish the exact OS event ordering.

Each scoped Send cache arms its deadline once from the first newer eligible committed `review.startedAt` in the same
active owned DEBUG probe, for the exact current card/run/conversation, plus three seconds. That start must be at or after
the pre-intent resume timestamp and no later than the observed clock. Missing, stale, foreign, closed or faulted evidence
cannot bind the cache. An expired first start refuses; initial binding cannot select a later reset or renew the bound window.
A cached read requires the same first snapshot's card, question object and selection in that active healthy session.
The latest eligible production start may equal the first start or move forward after a focus reset, only when it is no
later than the current elapsed clock and that clock is still below the original first start plus three seconds. Missing,
backward, future, overflowing or expired clocks refuse. The first snapshot, generation, start and end never change.
This admits only passive negative-input evidence during a more restrictive production reset. Positive Send still waits
for the ordinary current production review; no focus loss or reset is bypassed. Probe record `uptimeMs` values are
ordinal observations and are never compared with the elapsed clock used for review. The three retained JVM suites
contain 33 methods (8 publication, 18 binding and 7 review-observer controls); all 29 previous methods remain.
The read refreshes the same exact visible owned label and current card, then follows at
most sixteen ancestors in that window and package. Only an ancestor that fails `refresh()` permits another walk from that
same validated label; the passive poll is clamped to the remaining original deadline. A stale label, changed
card/window/package, missing action, read error, depth limit or late result refuses the read. There is no root search,
anchor rebinding, input retry or extra review time; the returned native node must still satisfy the original disabled
assertion. This reader's no-search rule follows the single post-resume preparation above. Android's
[`AccessibilityNodeInfo.refresh()`](https://developer.android.com/reference/android/view/accessibility/AccessibilityNodeInfo#refresh())
can return false when its represented View has left the tree; logical card/window continuity does not guarantee that a
pre-Home accessibility object remains live. The retained `95ef8f13` run failed in its first case's compound cached-label
guard, before a negative Send touch or any receipt. Its exact false operand remains unknown; a later owned window/card
snapshot does not identify it. Source-bound memory controls contrast an obsolete old reference with a current live label
under the same immutable identity/deadline, and retain refusal controls and the original disabled oracle. They are finite
stand-ins, not Kotlin compilation or demonstrated native recovery. The retained feedback failure identifies the
ancestor-loop refresh check, but not which iteration or node
failed. The `dd209de8` run entered its first assertion 3,214 ms after the resume intent while the last committed card still
reported three review seconds remaining and Send disabled. That deadline failure produced no disabled-node oracle and
did not reach the ancestor walk; it does not retrospectively qualify the ancestry repair.

Ordinary Home/resume can also initiate the production catalog refresh. The interceptor admits only GET
`/v1/capabilities` and `/v1/computers` at the exact reserved HTTPS origin, default port, empty userinfo/query/fragment/body,
and exact existing fixture authorization. Capabilities advertise only protocol1, `inspect`, `answer`, and no features.
Computers returns one synthetic computer and a synchronized clone of the current synthetic session/card, never an empty
or stale catalog. Each fixed path has at most64 observations with first/last monotonic timestamps. The original POST
receipt/answer guards and first-failure refusal remain unchanged; no request is forwarded. The observed prior
capabilities refusal does not establish the cause of a missing broader dialog. The broader case independently verifies
its exact selected draft, option2, empty feedback, card/run/conversation and native scope before the original Send.
Failure-only diagnostics inspect at most4 windows,128 nodes/16 levels and8 fixed labels per owned app window, with
160-character records. Foreign trees are omitted, unknown/overflow/refresh failures are diagnostic UNKNOWN, and the
original exception stays primary. No extra ADB subject or diagnostic success can qualify a failed/missing case.

Before dispatch, review this workflow and the SDK-free `tests/test_android_project_ui.py` and
`tests/test_approval_ui_harness.py` controls. The new app/test are built under a fresh synthetic debug signer; this does
not establish compatibility with the operator's installed signer. Do not reuse an old private D01/ISO resource grant,
APK, AVD, profile, key, lease, 22-subject producer, or result.

## Desktop: real review, feedback and second click

On an isolated hosted Qt checkout, run the normal `test_questioncard` suite, including
`kimiNativeChoicesReviewFeedbackAndReplacementGuards`, `claudeApprovalListsNativeOptionsAndConfirmsBroaderOnes`,
`reviewCountdownAndNextNavigation`, and `nativeCountdownPreservesScrollChoiceAndReview`. The new test does not shorten
the production review delay. It checks hide/show resets the review, first-click zero callbacks, explicit second-click
payload, changing choices cancels confirmation, a new hash cannot consume old confirmation, early Enter cannot send,
feedback has no selected native option, and a read-only replacement stays locked. This does not run a native provider.

## iOS: separate unsigned simulator fixture

Use only a fresh standard GitHub-hosted macOS VM. Root must select and record the exact Xcode build, installed simulator
runtime identifier and device-type identifier before a separate execution grant. The example variables below are
required reviewed inputs, not fallback values. Do not install a new runtime, select a physical device, use a distribution
environment, import a signing identity, or access Keychain. No product Xcode target or scheme is changed.

Create a fresh private work directory, then generate the project once:

```sh
test "$RUNNER_ENVIRONMENT" = github-hosted
test "$(git rev-parse HEAD)" = "$EXPECTED_SOURCE"
test -z "$(git status --porcelain)"
umask 077
gate=$(mktemp -d "$RUNNER_TEMP/approval-ios.XXXXXXXX")
python3 -I -B scripts/ci/ios-approval-ui-project.py "$gate/project"
```

The generator refuses an existing output. It references production source files directly, records their hashes and the
local package sources, and creates only an iOS-simulator application and one UI-test target. Its package is entirely
local. Record `xcodebuild -version`, `xcrun simctl list runtimes --json`, and `xcrun simctl list devicetypes --json`, and
refuse unless the separately reviewed exact build/runtime/device inputs are available. No search or latest fallback is
admitted. Create exactly one new simulator and retain its returned UUID as the sole lifecycle target:

```sh
simulator=$(xcrun simctl create "Zerus approval $GITHUB_RUN_ID" "$EXPECTED_DEVICE_TYPE" "$EXPECTED_RUNTIME")
xcrun simctl boot "$simulator"
xcrun simctl bootstatus "$simulator" -b
xcodebuild -project "$gate/project/ApprovalUIFixture.xcodeproj" -scheme ApprovalUIFixture \
  -configuration Debug -sdk iphonesimulator -destination "platform=iOS Simulator,arch=arm64,id=$simulator" \
  -derivedDataPath "$gate/derived" -clonedSourcePackagesDirPath "$gate/packages" \
  -packageCachePath "$gate/package-cache" -disableAutomaticPackageResolution -disablePackageRepositoryCache \
  -parallel-testing-enabled NO -maximum-concurrent-test-simulator-destinations 1 -jobs 2 \
  -test-timeouts-enabled YES -default-test-execution-time-allowance 30 \
  -maximum-test-execution-time-allowance 30 -resultBundlePath "$gate/result.xcresult" \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY= test
```

The executable gate is `scripts/ci/ios-approval-ui.py` through `ios-approval-ui.yml`. A PR touching this gate or its exact
production source inputs selects the PR head with the reviewed fixed Xcode 26.3/build 17C529, iOS 26.2 and iPhone 16.
Manual dispatch retains explicit required inputs. Both paths refuse absent or mismatched installed selections without
fallback, use read-only credentials and no Actions caches, and run only on a disposable hosted VM. Review the workflow before
publishing its PR; the shell lines above explain its commands and are not an alternative bypass. The driver requires the
exact selected `DEVELOPER_DIR` and `EXPECTED_XCODE_BUILD`, not the existing product selector's newest-Xcode fallback.
It refuses a non-arm64 hosted process before allocation, selects the arm64 simulator destination explicitly, and builds
the separate Debug fixture with `ONLY_ACTIVE_ARCH=YES`, matching the production Debug configuration and local package.
Apple tools retain the VM's validated standard `HOME`, `TMPDIR`, `USER`, and `LOGNAME`; those exact environment values
are recorded with command timings. Evidence, DerivedData, package outputs and XDG paths remain private. The standard
hosted Apple account and its caches are in scope; an artificial empty home is not an isolation guarantee. Earlier
CoreSimulator inventory timeouts remain setup/unknown outcomes, with no proven cause attributed to the home directory.
The first cold `simctl list runtimes` has a 120-second cap without retry; later work inventory keeps its existing caps.
The driver imposes one original 900-second budget and one cleanup phase ending at the earlier of that original end or
60 seconds after cleanup first begins, including an earlier forced owned-child stop. That phase is never renewed.
The two fresh cleanup identity/state inventories have 15-second caps, shutdown has 10 seconds and delete has 8 seconds,
all within that same cleanup end. Exact created UUID/name/runtime/state guards and native command order stay unchanged.
The driver bounds boot to 120 seconds and build/test to
600 seconds or the remaining shared budget, caps raw output at 32 MiB per command, owns the unreaped direct child before
later fallible calls, and retains failures without retry. A forced child stop is failed/unknown, never passing cleanup
evidence. The exact native assertions are the seven
methods of `ApprovalChoicesUITests`: countdown/once, broader cancel/confirm, card replacement, unavailable replacement,
feedback keyboard, actual background/foreground, and hidden/read-only cards. Require seven executed tests, zero errors,
failures or skips; inspect the raw XCTest results and callback assertions, rather than accepting exit zero alone.
Keyboard and dialog behavior must be observed, not inferred from the existing SwiftPM policy tests.

Both disposable targets import the public Apple `<notify.h>` declarations through the same fixed fixture-only bridging
header. Its bytes join the exact project source identities. The production application does not reference this header;
no handwritten C ABI, SDK-header replacement, or new runtime dependency is introduced.

The four fixture-only control modes use Darwin notifications through `notify_register_dispatch`, `notify_post`, and
`notify_cancel` ([Apple API header](https://raw.githubusercontent.com/apple-oss-distributions/Libnotify/main/notify.h),
[notification semantics](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/notify.3.html)).
The shared namespace does not authenticate a sender. This channel is confined to the disposable simulator fixture:
exact names bind a fresh test-launch UUID and a separate random app-generation UUID, with only the review action needed by
the chosen mode registered. Every mode also registers one terminal `close-controls` action for owned teardown. After the
original case assertions, the test posts it once; it cancels its own token before closing the remaining registrations.
The actual visible `closed` acknowledgement must arrive within the unchanged two-second limit before termination;
posting success alone cannot acknowledge cleanup. The visible Close controls button remains available, but automated
registration cleanup does not depend on native tap delivery. Each review action likewise consumes and cancels its own
token once, and the UI must acknowledge its action/serial. Explicit fixture closure checks cancellation before termination;
a failed registration, acknowledgement, timing precondition, or cleanup cannot pass. No production target uses this API.

Countdown mode additionally starts unavailable, selects the original native option while ordinary card inputs remain
editable, then admits only its one-time `begin-countdown-review` action. That action sets fixture availability true;
the exact production eligibility path starts the unchanged review. No Home/activation latency or artificial clock is
part of this countdown fixture. Feedback and countdown use a fixed reverse notification bound to the launch UUID,
actual app generation, a separate fresh ACK UUID and a fixed action code. A main-actor UITest receiver registers before
post, consumes/cancels its exact token once, and acknowledges through an XCTest expectation. Its checked teardown block
runs even after a failing test. The app commits the original visible ACK before posting the reverse ACK; posting alone
is not proof. A monotonic timestamp before the original action post is a conservative lower bound for review start.
The original disabled/zero or real short edit/Return must finish below three seconds measured from that timestamp;
only afterward is the unchanged visible action/serial/time/generation ACK read. There is no pre-action accessibility
ACK poll. A late or missing callback, cancellation, visible ACK or timing guard fails without a bypass claim.

Feedback mode seeds only the fixed memory draft prefix and keeps the synthetic card unavailable while the real keyboard
opens. The one-time `begin-feedback-review` notification makes that card available through its ordinary production
eligibility path; a real short edit and Return must finish inside the original three-second window and leave zero
callbacks. After the real review, another Return must record exactly the original answer. A missed early window fails
the fixture without a bypass claim. The early Return can resign native keyboard focus. After its original timing/ACK/
zero-callback proof, the test checks the full retained text, taps that actual field once, checks unchanged text and zero
callbacks again, then runs the original final review/Return and exact payload assertions. It does not replace the text
or assume [`typeText`](https://developer.apple.com/documentation/xcuiautomation/xcuielement/typetext(_:)) focuses a field.
Replacement mode posts its one-time `replace` only after native Confirm is observed,
then requires the same native retirement and no-submit assertions. The broad disclosure check expects the entire
production message, including its description. Unavailable mode likewise observes the original native Confirm, full
disclosure and zero callbacks after the real review and Send, then posts its one-time `become-unavailable` event. It sets
only the fixture snapshot's availability false through the existing mode/generation/token guard. The original
four-second Confirm-retirement, zero-callback and disabled-Send assertions remain mandatory, together with the exact
action/serial/generation acknowledgement and checked closure. A physical timer trigger before Send allowed the first
Confirm lookup to occur after its two-second window; that failure did not reach the retirement oracle. No timer,
longer observation cap or retry substitutes for observing the original dialog before this event.
These controls never set a review clock, foreground flag, native dialog,
or provider permission. Monotonic phase records and exact ACKs are evidence only; all seven UI tests still have to pass.

Broader cancellation first uses an actual native Cancel when present. If absent, the test requires exactly one actual
native popover containing Confirm and the full production disclosure, a recognized actual horizontal size class, and
a positively identified passive fixture header in the same unique owned app window. The popover, header, window and
app canvas must have positive finite rectangles. Clip the actual header rectangle to that window and app canvas, then
derive the first positive region above, below, left or right of the actual popover. Full occlusion or an empty clip
fails. The centre of this unoccluded region supplies a strictly interior normalized offset relative to the actual
header; its dynamic [`XCUICoordinate`](https://developer.apple.com/documentation/xcuiautomation/xcuicoordinate) must resolve
to a finite screen point strictly inside that region, header, window and app canvas, and outside the popover, before
one physical tap. All original rectangles and the actual size class are logged before geometry admission, and each
geometry criterion records its result before an assertion can abort. A partially overlapping header rectangle alone
does not reject a proven visible outside point. The prior combined positive-size/nonintersection failure printed no
rectangles, so its exact failed conjunct remains unknown; this source change makes no native-success or cause claim.
Background `isHittable` is deliberately not an admission predicate for this outside-dismissal gesture: Apple documents
that [popover presentation](https://developer.apple.com/documentation/uikit/uipopoverpresentationcontroller/passthroughviews)
normally disables underlying view interaction. [SwiftUI confirmation dialogs](https://developer.apple.com/documentation/swiftui/view/confirmationdialog(_:ispresented:titlevisibility:presenting:actions:)-9ibgk)
use an outside tap instead of the standard dismiss action for regular-size popovers. This replaces a new fixture-only
guard that failed before the actual geometry/tap; original product and callback oracles are unchanged. Unknown role/geometry/traits
fails instead of guessing a screen point or altering presentation. Confirm retirement and zero callbacks are required
before the original separate Send/Confirm and final option2 payload. This path is source-only until actually qualified.

The configured XCTest default/maximum allowance stays30 seconds. Apple documents that
[`executionTimeAllowance`](https://developer.apple.com/documentation/xctest/xctestcase/executiontimeallowance) rounds to
minute granularity; a reported43.737-second case alone does not prove a violation of the configured value. The original
900-second driver end, the 60-second cleanup phase and UI/boot/build caps remain unchanged. The two cleanup metadata caps
are reallocated from 8 to 15 seconds within that shared phase; they do not extend the whole or cleanup end. A timeout does
not establish the underlying host cause. Command deadlines are fixed before spawning and cover persistence and final close.
Final outcome persistence must also meet the cleanup end before the driver returns zero; a partial outcome JSON alone
cannot qualify a campaign whose final deadline or process cleanup failed.

Retain the source map, raw stdout/stderr, result bundle and actual test inventory before cleanup. Re-hash every source
entry and the built fixture products. Shut down and delete only the newly returned simulator UUID after matching its
name/runtime to the recorded creation. An unknown shutdown, changed identity or surviving runner is a failed/unknown
gate, with evidence retained; never use `shutdown all`, broad process kills or numeric-PID fallbacks. This gate makes no
all-descendant, native-keychain, production-model, provider, operator-device or installed-delivery claim. Xcode's shared
SwiftPM cache behavior on the disposable VM is not a hermeticity claim.

The Android fixture distinguishes its interceptor observing an answer request from production applying that receipt.
After each of the four existing request-count waits, it requires the current exact target/card/run/conversation,
the recorded request ID and full answers in a submitted draft, no answer flight or pending consent, and an idle verified
detail read before advancing. This reads ordinary production state; it neither acknowledges nor fabricates a receipt.
The intentionally unsent old replacement card has no such receipt gate. Every broader Send also proves its saved option2
and empty-feedback draft before the existing real touch. Refreshed touch window/state/bounds/centre records are bounded
diagnostics, not proof that a touch activated a control. Failure window evidence and the existing screenshot capture run
before the Activity's back/finish cleanup; diagnostic or cleanup errors cannot replace the original throwable. The driver
does not retrieve a PNG or add any host operation. These fixture admission/diagnostic corrections do not establish the
cause of the earlier missing-modal failure or a product defect; all seven cases, original actions and budgets remain.

The later replacement fixture also proves the existing exact option3/empty/editing/current draft after its one Reject
touch, before Home, and rechecks it only after the unchanged early timing assertion has completed. It never creates a
draft or repeats the touch. Failure-only observations record real Activity focus/lifecycle, existing draft completeness,
and exact fixed synthetic command/title/review labels with owned root, scroll and window bounds. The active owned root
is observed before a separately caught window-list query; a failed query/traversal retains earlier lines and its stage.
The evidence is capped at64 lines of160 characters, with128 visited nodes/depth16,12 fixed-label matches and4 scroll
nodes per root, and at most4 listed windows. These records cannot qualify a case or select an answer. They distinguish
missing selection from unavailable review evidence without overriding the production timer or inferring a cause from a
disabled Send alone. Current-window/focus admission, additional disclosure scrolling and every host operation remain
unchanged; the observed replacement failure remains unproven until actual bounded evidence identifies its cause.

The Android scroll hint now occupies a measured sibling row outside the weighted disclosure/option box, rather than
overlaying it. Its Surface, colors, label, padding and conditional visibility remain; while shown, its measured height
and the existing 6dp column spacing reduce the inline or expanded disclosure viewport. When hidden, no row is reserved.
Review visibility, scroll state/padding, choices, feedback, footer and confirmation policies are unchanged. Material3
Surface blocks touches behind its bounds, including its non-clickable form; the declared 2025.06.01 BOM maps to 1.3.2.
The 1f0466cd attempt proved missing Reject selection near the hint text, but did not retain the old Surface background
rectangle. Historical exact-pixel attribution remains an inference; unchanged physical fixture/native acceptance is
still required. SDK-free brace-scope controls prove the sibling relationship, not Compose measurement or touch behavior.

The Android fixture observes only its existing physical Send intervals through a delegated callback on its exact
owned Activity window. Touch and focus callbacks forward their original arguments once, preserve the original return
or exception, and keep bounded before/after memory records: at most eight Send tokens and four events per token.
Screen-coordinate targets bind to received `rawX/rawY`; the received event object is distinct from the injector's
object. Window identity is read outside dispatch; dispatch performs no window-tree/Binder query or additional action.
The current Compose-local review guard remains unknown. A consumed window event does not prove the named button
clicked. Mismatched identity/metadata, recording faults, changed callbacks or overflow make the observation unknown.
Restoration checks the exact installed callback and is attempted independently of the original back/finish cleanup.
These source-bound memory controls are not Kotlin, Android OS dispatch or a fix for the unproven f0c Send failure.
Both native outcomes retain the final bounded memory trace. A positive result appends it on the original single
stream line, preserving the unchanged seven-case parser; a failure retains the original error and diagnostic stream.

## iOS read-visibility campaign

`ios-approval-ui.yml` also runs `scripts/ci/ios-read-visibility-ui.py` in a second job, `read-visibility-ui`. It runs
on its own disposable hosted VM with the same triggers, reviewed inputs, admission, replica build, deadlines and cleanup,
so it neither shares nor enlarges the approval campaign's limits. The runner and `ios-read-visibility-ui-project.py`
are the approval ones with only the fixture identity changed; `tests/test_read_visibility_ui_harness.py` pins that
derivation, the job, the one native case and the generated project.

The fixture (`mobile/ios/ReadVisibilityUIFixture`, zerus-a4mub.31) compiles the unchanged
`ConversationReadLayout.swift`, which reports each list item's placement from `bounds(of: .scrollView)`, and ZerusKit's
readable-visibility rule, read state and unread count. Synthetic rows, exact-offset scroll controls and the settled
350 ms dwell copied from `ConversationView` show the following on screen:
- a message scrolled fully into view is read after the dwell;
- a 20-point sliver of it is not;
- a questions pane taller than twice the viewport counts once its 48-point readable part is visible.

Its bundle identifiers are `dev.zerus.fixture.readvisibility` and `.uitests`, because the iOS 26.0 simulator runtime
refused to install a `test.example.zerus.*` identifier (zerus-a4mub.34).
