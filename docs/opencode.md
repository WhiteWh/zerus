# OpenCode in Zerus

[Reference](reference.md) | [Local models](local-models.md)

OpenCode owns its tools, permissions and conversations. Zerus connects to
OpenCode 2's native HTTP API and presents that conversation in desktop Activity,
on Android and on iOS. The model provider is configured on the computer running
OpenCode; the client and mobile relay do not run inference or receive provider
credentials.

## Supported versions

Zerus supports OpenCode **2.0.32** or newer 2.x. Check `opencode --version` on
the agent computer. Before using a running service, Zerus reads its
`/openapi.json` and requires every operation it uses. An incompatibility is
reported as one of:

- `OpenCode <version> lacks <method> <path>; update OpenCode or Zerus`
- `OpenCode <version> is not supported; install OpenCode 2.0.32 or newer 2.x`

Zerus does not upgrade, stop or replace OpenCode.

## The shared service

OpenCode 2 runs one background service per user, registered in
`$XDG_STATE_HOME/opencode/service.json`. Zerus uses that service and never
stops, restarts or replaces it, so other OpenCode clients are unaffected.

Zerus starts it (`opencode service start`, in its own transient user scope on
Linux so that it outlives Zerus) only when no service is registered or the
registered process is dead, never while an existing registration cannot be
read, and only for a launch or the explicit **Start
OpenCode** action (`hgs account start native-opencode`). Inspection never starts
it. Without a registered live service, Zerus shows "OpenCode is not running on
<computer>" and offers Start OpenCode; a registered service that does not answer
shows "OpenCode is not responding" and is never restarted. When OpenCode's
background service is disabled, Zerus shows "The OpenCode background service is
disabled on <computer>" and starts nothing. `<computer>` is the name `hgs ls` reports as
its host: the first `HGS_SELF` name (by default the host name), which `hgs
machine` setup sets to the desktop's name for that computer.
You can also start it yourself with `opencode service start`.

For previously unmarked conversations that already have turns, OpenCode 2.2.7
or newer can receive Zerus's optional coordination hint through a context plugin.
Zerus supplies it only in the environment of a service it starts, preserving the
person's config files and plugins. Each primary request checks its own decided
session binding; native subagents, stale or busy bindings and opted-out sessions
receive no new context. Restarting a managed service and resuming verifies the
plugin again. An already-running service without it is left running and reports
the hint as unavailable; Zerus does not restart it or install into global plugin
folders. Fresh/still-empty conversations retain their existing instruction-entry
channel, and an entry-backed conversation never receives a second plugin hint.

Connect providers in a terminal on that computer with `opencode auth login`.
`hgs account login native-opencode` does not run it; it prints that instruction.

OpenCode service refusals can contain private provider diagnostics. Local
inspection keeps the bounded detail for troubleshooting on the session's
computer. Phone listings and inspections show a fixed explanation instead;
ordinary conversation text and questions are unchanged. Failed or uncertain
session actions use the same fixed error in their durable receipt and every
retry. Delivery refusal records retain only the HTTP status; the first local
CLI rejection may show the service's bounded detail on stderr. Historical
receipts already recorded by an older installation are not rewritten.

## Models

Zerus lists the models the service reports (`/api/model`), through an allowlist:
keys, URLs and headers never leave the computer. Keyless free models such as
`opencode/glm-5.3-flash-free` and `opencode/deepseek-v4.1-flash-free` are listed
when the service offers them. Effort is the selected model's variants. The
service's default model is always offered. Models declared in a project's own
`opencode.json` appear for that project folder.

The account row lists a short list, not every model: the default model, then your
favorites, then the models of providers you have configured, then free models.
Each model appears once and the list holds at most 256 entries; when it is cut,
`model_options_truncated` is true. Favorites are stored on the agent computer
(`opencode-favorites.json` beside the hgs account catalog, at most 64). A
favorite that OpenCode no longer lists stays until you remove it. The desktop's
All models shows the full catalog.

The row also lists up to 64 providers and stays within 32 KiB (measured as
`json.dumps(row, ensure_ascii=True)`). Over that budget hgs drops short-list
entries from the end (free, then configured, then favorites) while keeping the
default model, then providers beyond the first 16, then the remaining providers
until only the default model is left, and reports `model_options_truncated` and
`providers_truncated`. A session's model picker
uses its folder's list plus its current model. Phones list only model values of
at most 120 characters, and only providers whose IDs use ASCII letters, digits,
`_`, `.` and `-` (at most 80 bytes); choose other models on the desktop or with
`hgs`.

### Configure a local model

Install Ollama and load a model that supports tools on the same computer.
`ollama list` shows the existing models. The local example in this repository
uses `qwen3-coder:30b`; embedding models are not coding agents.

Merge this provider into OpenCode's configuration, normally
`~/.config/opencode/opencode.json` (or `opencode.jsonc`), or into the project's
`opencode.json`. Retain other providers, credentials and permission rules.

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "ollama": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Ollama (local)",
      "options": { "baseURL": "http://127.0.0.1:11434/v1" },
      "models": {
        "qwen3-coder:30b": {
          "name": "Qwen3-Coder 30B",
          "tool_call": true,
          "temperature": true,
          "limit": { "context": 32768, "output": 2048 }
        }
      }
    }
  }
}
```

This adds an explicitly selectable `ollama/qwen3-coder:30b`; it does not change
the native default model. `temperature: true` declares model capability, not a
numeric temperature. Choose a model and context that fit the available GPU
memory while other local services remain running. See the
[OpenCode provider documentation](https://opencode.ai/docs/providers/#ollama).

Zerus preserves model replies that contain literal tool markup as text; it never
executes tool-like prose. A local model's tool-calling reliability varies. The
[Codex + Ollama example](local-models.md) has separate successful edit, test and
exact-resume evidence.

## Start a conversation

In New session, select the computer, **OpenCode**, its native account and an
offered model, and an effort where the model has variants. A blank model uses
the service default. Inspecting the form creates neither a worktree nor a native
session; only Start session does. Phones use the same form and the same Zerus
run and native conversation identity.

From the CLI (`--effort` requires `--model`):

```sh
hgs opencode /workspace/orbit --new -n local-review --model opencode/glm-5.3-flash-free
hgs opencode /workspace/orbit --new -n deep-review --model opencode/glm-5.3-flash-free --effort high
hgs @build-01 opencode /workspace/orbit --new -n local-review --model ollama/qwen3-coder:30b
hgs inspect opencode/orbit/local-review
```

The project directory remains on the agent computer.

## Controls and limits

- **Send.** Messages sent while OpenCode is working queue. After Stop, a queued
  message shows "Not delivered": it is still waiting in OpenCode's queue. When
  you send your next message, OpenCode runs the waiting one first, with its own
  reply, and then yours, unless you remove it first (Remove asks for
  confirmation). "Not delivered" is not a refusal, and Zerus never resends the
  message on its own.
- **Stop** interrupts the current turn.
- **Questions.** Option, boolean and text fields are answered in Zerus; other
  forms show "Answer this form in OpenCode".
- **Permissions.** Allow once, Always allow or Reject. Reject rejects every
  pending request of that session. Always allow asks for a second confirmation.
- **Model and effort** can be changed; **rename** changes the session title.
- **Forget** removes the Zerus binding and its local history cache; native
  history is kept. The cache keeps at most 64 recently used conversations and
  256 MiB.
- **Voice** transcripts use the same guarded send path; a refused voice message
  shows as failed, and the original audio stays in Zerus history.
- **Terminal** on desktop opens the native OpenCode TUI on the existing service
  and conversation. The CLI equivalent is `hgs a NAME --existing --run-id RUN`.
  Closing the client keeps that service and conversation running; opening it
  never starts or replaces a service and disables the client's automatic updates.
  Mobile pane snapshots remain unavailable.

Not available in this version: pause/resume (OpenCode sessions have no separate
process to pause), attachments, compaction, fork and child sessions
(a later update).

### Delivery

Zerus pre-generates each message ID. If a response is lost, it reconciles the
exact message (message list, then inbox) instead of guessing, and never posts
the message again, so a message removed from the queue cannot come back. A 4xx
reply is a definite refusal and is never resent. Removing a queued message is
recorded before OpenCode is asked; once OpenCode confirms the removal, the
request reports "Removed from the OpenCode queue before delivery". Interrupted
or failed delivery, or a removal OpenCode did not confirm, is never resubmitted
automatically; an uncertain result stays for inspection.

## OpenCode 1 conversations

Zerus no longer supports OpenCode 1. After OpenCode 2 migrates an old
conversation, Zerus adopts it automatically (same session ID and folder).
Otherwise it is listed as "This conversation belongs to OpenCode 1, which Zerus
no longer supports" with Forget only. Native data is never modified.

## Privacy

The service password, URL and registration file never leave the computer: they
are not relay data or public catalog fields. Catalog fields are allowlisted.
Closing or updating Zerus does not stop the service.

## Qualification

The ordinary CLI suite covers parsing and projections. The native adapter suite
runs against the actual OpenCode 2 binary with private HOME and XDG folders, its
own service on a private port and a keyless local model; it never touches the
service, settings or sessions of the person running it and does not contact paid
model APIs. Full Linux and macOS CI and the daily upstream report each test one
latest release, while product admission retains the supported minimum
([native agent compatibility](ci-and-aur.md#native-agent-compatibility-and-upstream-review)).
Run it explicitly with OpenCode installed:

```sh
cargo build --locked
ZERUS_TEST_OPENCODE_NATIVE=1 HGS_TEST_BIN="$PWD/target/debug/hgs" \
  PYTHONPATH=tests python3 -m unittest test_opencode_native -v
```

With `ZERUS_TEST_OPENCODE_NATIVE=1`, a missing OpenCode or one older than the
minimum fails the suite; `ZERUS_TEST_OPENCODE_BIN` selects another executable.

The native suite also covers the provider catalog. A configured model ID that
contains `/` is checked first, before any newer catalog field or command, and
is then launched, changed to and sent a turn under. The suite checks
keyless free models, the default model first, allowlisted provider fields, and
a provider that only one folder configures, which stays out of the home folder,
the account row and other folders, also after that folder was read and launched
in. A stopped service keeps the last short list without being started.
Favorites change nothing in OpenCode's files, and a request identity reused for
another model or flag is refused. A recording proxy in front of the private
service shows that the catalog reads, inspection and favorites the suite makes
never ask OpenCode for credentials, and that the fixture's key and base URL,
which OpenCode does return, never reach hgs output.

The suite turns off OpenCode's model fetch, autoupdate and LSP, but that does
not make OpenCode hermetic: it can still look for local model servers such as
Ollama or reach the network on its own. To exclude that, run the suite in a
network namespace that has only loopback.

See [contributing](../CONTRIBUTING.md) for the common CLI, Qt and Android
gates. A skipped suite is not compatibility evidence.
