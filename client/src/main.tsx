import { render } from "solid-js/web";
import { createSignal } from "solid-js";
import "./ui/styles/base.css";
import "./ui/styles/app.css";
import "./ui/styles/sessions.css";
import "./ui/styles/detail.css";
import "./ui/styles/client.css";
import "./ui/styles/goto.css";
import "./ui/styles/screens.css";
import "./ui/styles/machines.css";
import "./ui/styles/motion.css";
import "./ui/styles/settings.css";
import "./skin/runtime";
import { App } from "./App";
import { inTauri, startBridge, type BridgeMode } from "./bridge";
import { invoke } from "@tauri-apps/api/core";
import { receive } from "./store/session";
import { startDesk } from "./store/desk";
import { startDrops } from "./store/drops";
import { startUpdates } from "./store/updates";
import { composer } from "./store/composer";
import { exitFlush } from "./store/exitFlush";
import { startMotion } from "./ui/motion";
import { platform } from "./store/shortcuts";
import { loadMarkdown } from "./ui/sessions/markdown";

const [mode, setMode] = createSignal<BridgeMode | "starting">("starting");

// The motion and platform attributes are on the root before the first frame.
document.documentElement.dataset.platform = platform;
exitFlush.register(composer.flushForExit);
void exitFlush.start().catch((error) => console.warn("Zerus Next: exit flush listener did not start", error));
startMotion();
// The Markdown parser is its own chunk; Activity waits for it before placing rows.
void loadMarkdown();
render(() => <App mode={mode} />, document.getElementById("root")!);
// The menu bar and notifications open sessions and show notices in this window.
startDesk().catch((error) => console.warn("Zerus Next: the menu bar wiring did not start", error));
startUpdates().catch((error) => console.warn("Zerus Next: the updates wiring did not start", error));

void startBridge(receive).then(setMode, (error) => console.error("Zerus Next: the data source did not start", error));
// Files dropped on the session's panel go to its composer or its Terminal.
startDrops();

// Load the bounded device-local skin library once; no directory poller.
void import("./skin/user").then(m => m.loadUserSkins());

// The second frame after load reached the page: a DMA-BUF start worked, so its crash marker goes.
if (inTauri()) {
  const frame = () => requestAnimationFrame(() => requestAnimationFrame(() => { invoke("renderer_frame").catch(() => {}); }));
  if (document.readyState === "complete") frame();
  else window.addEventListener("load", frame, { once: true });
}
