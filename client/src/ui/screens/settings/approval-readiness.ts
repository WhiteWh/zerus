import type { DesktopService } from "../../../types";

/** Display affordance only. The native route and service still enforce authority. */
export function canToggleServiceApproval(service: Pick<DesktopService, "connected" | "automatic_approvals" | "background"> | undefined): boolean {
  return !!service?.connected && typeof service.automatic_approvals === "boolean"
    && (service.automatic_approvals || service.background.approvals?.ready === true);
}
