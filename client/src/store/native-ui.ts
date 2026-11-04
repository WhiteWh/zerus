import * as bridge from "../bridge";
import { createNativeUi } from "./native-ui-model";
import { installNativeOverlays } from "./native-overlays";

let embedded = false;
export const nativeUi = createNativeUi({
  open: (request, reload) => bridge.nativeUiOpen(request, reload, embedded), retire: bridge.nativeUiRetire, visible: bridge.nativeUiVisible,
  bounds: bridge.nativeUiBounds, browser: bridge.nativeUiBrowser,
});
embedded = installNativeOverlays(nativeUi);
void bridge.onNativeUi(nativeUi.apply);
