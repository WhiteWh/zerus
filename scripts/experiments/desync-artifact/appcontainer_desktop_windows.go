package main

// Window station and desktop access for contained jobs. A contained process
// inherits the executor's window station and desktop; user32 opens both while
// the process initializes, and without access it ends with
// STATUS_DLL_INIT_FAILED (0xC0000142) before running anything. An interactive
// desktop already admits AppContainers. The shared service runs as a batch
// logon whose private, non-interactive window station does not, so the
// installation's jobs capability (the same SID that lists the executor's
// folders) receives the minimal rights on that window station and desktop,
// once: an existing entry is kept.

import (
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	user32                      = windows.NewLazySystemDLL("user32.dll")
	procGetProcessWindowStation = user32.NewProc("GetProcessWindowStation")
	procGetThreadDesktop        = user32.NewProc("GetThreadDesktop")
)

const (
	// winuser.h: enumerate desktops, read attributes, global atoms, enumerate.
	windowStationRights = 0x0001 | 0x0002 | 0x0020 | 0x0100 | windows.READ_CONTROL
	// winuser.h: read objects, create windows and menus, enumerate.
	desktopRights = 0x0001 | 0x0002 | 0x0004 | 0x0040 | windows.READ_CONTROL
)

// ensureDesktopAccess lets the plan's jobs capability use the executor's
// window station and desktop.
func ensureDesktopAccess(plan containerPlan) error {
	var jobs *windows.SID
	for i, name := range plan.CapabilityNames {
		if len(name) >= 9 && name[:9] == "zerusJobs" && i < len(plan.Capabilities) {
			jobs = plan.Capabilities[i]
		}
	}
	if jobs == nil {
		return accessCode("container-plan-invalid")
	}
	station, _, _ := procGetProcessWindowStation.Call()
	if station == 0 {
		return accessCode("container-desktop-unavailable")
	}
	if err := grantWindowObject(windows.Handle(station), jobs, windowStationRights); err != nil {
		return err
	}
	desktop, _, _ := procGetThreadDesktop.Call(uintptr(windows.GetCurrentThreadId()))
	if desktop == 0 {
		return accessCode("container-desktop-unavailable")
	}
	return grantWindowObject(windows.Handle(desktop), jobs, desktopRights)
}

func grantWindowObject(object windows.Handle, sid *windows.SID, rights uint32) error {
	sd, err := windows.GetSecurityInfo(object, windows.SE_WINDOW_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		return accessCode("container-desktop-unreadable")
	}
	dacl, _, err := sd.DACL()
	if err != nil {
		return accessCode("container-desktop-unreadable")
	}
	if dacl != nil && windowObjectGranted(dacl, sid, rights) {
		return nil
	}
	merged, err := windows.ACLFromEntries([]windows.EXPLICIT_ACCESS{{
		AccessPermissions: windows.ACCESS_MASK(rights),
		AccessMode:        windows.GRANT_ACCESS,
		Inheritance:       windows.NO_INHERITANCE,
		Trustee: windows.TRUSTEE{
			TrusteeForm:  windows.TRUSTEE_IS_SID,
			TrusteeType:  windows.TRUSTEE_IS_WELL_KNOWN_GROUP,
			TrusteeValue: windows.TrusteeValueFromSID(sid),
		},
	}}, dacl)
	if err != nil {
		return accessCode("container-desktop-grant-failed")
	}
	if err := windows.SetSecurityInfo(object, windows.SE_WINDOW_OBJECT, windows.DACL_SECURITY_INFORMATION, nil, nil, merged, nil); err != nil {
		return accessCode("container-desktop-grant-failed")
	}
	after, err := windows.GetSecurityInfo(object, windows.SE_WINDOW_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		return accessCode("container-desktop-unreadable")
	}
	if acl, _, err := after.DACL(); err != nil || acl == nil || !windowObjectGranted(acl, sid, rights) {
		return accessCode("container-desktop-grant-mismatch")
	}
	return nil
}

// windowObjectGranted reports an allow entry for sid holding every right.
func windowObjectGranted(dacl *windows.ACL, sid *windows.SID, rights uint32) bool {
	for i := 0; i < int(dacl.AceCount); i++ {
		var ace *windows.ACCESS_ALLOWED_ACE
		if windows.GetAce(dacl, uint32(i), &ace) != nil || ace.Header.AceType != windows.ACCESS_ALLOWED_ACE_TYPE {
			continue
		}
		if (*windows.SID)(unsafe.Pointer(&ace.SidStart)).Equals(sid) && uint32(ace.Mask)&rights == rights {
			return true
		}
	}
	return false
}
