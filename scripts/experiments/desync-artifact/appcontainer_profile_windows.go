package main

// AppContainer profile ownership. A profile is used only when the executor
// root holds its own origin record for that exact name, written durably
// before the profile is created. A profile that exists without such a record
// is a collision and is never adopted. Deletion requires the matching record;
// callers delete only after every job that used the container has settled.

import (
	"errors"
	"os"
	"path/filepath"
	"unsafe"

	"golang.org/x/sys/windows"
)

const hresultAlreadyExists = 0x800700b7

// containerOrigin is the durable record of a profile this executor created.
type containerOrigin struct {
	Protocol     int    `json:"protocol"`
	Name         string `json:"name"`
	SID          string `json:"sid"`
	Installation string `json:"installation"`
	Host         string `json:"host"`
	Person       string `json:"person"`
	Context      string `json:"context"`
	State        string `json:"state"` // creating, created, collision
	Build        string `json:"build"`
}

// profileAPI is the AppContainer profile store; production is the Windows
// userenv API. Tests use a fake: no profile is created on a developer
// computer.
type profileAPI interface {
	create(name string) (sid *windows.SID, exists bool, err error)
	remove(name string) error
	folder(sid *windows.SID) (string, error)
}

type windowsProfiles struct{}

var (
	procCreateAppContainerProfile = userenv.NewProc("CreateAppContainerProfile")
	procDeleteAppContainerProfile = userenv.NewProc("DeleteAppContainerProfile")
	procGetAppContainerFolderPath = userenv.NewProc("GetAppContainerFolderPath")
)

func (windowsProfiles) create(name string) (*windows.SID, bool, error) {
	p, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return nil, false, err
	}
	description, _ := windows.UTF16PtrFromString("Zerus executor job container")
	var sid *windows.SID
	hr, _, _ := procCreateAppContainerProfile.Call(uintptr(unsafe.Pointer(p)), uintptr(unsafe.Pointer(p)), uintptr(unsafe.Pointer(description)), 0, 0, uintptr(unsafe.Pointer(&sid)))
	if hr == hresultAlreadyExists {
		return nil, true, nil
	}
	if hr != 0 {
		return nil, false, errors.New("profile creation failed")
	}
	defer procFreeSid.Call(uintptr(unsafe.Pointer(sid)))
	copy_, err := sid.Copy()
	return copy_, false, err
}

func (windowsProfiles) remove(name string) error {
	p, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return err
	}
	if hr, _, _ := procDeleteAppContainerProfile.Call(uintptr(unsafe.Pointer(p))); hr != 0 {
		return errors.New("profile deletion failed")
	}
	return nil
}

func (windowsProfiles) folder(sid *windows.SID) (string, error) {
	p, err := windows.UTF16PtrFromString(sid.String())
	if err != nil {
		return "", err
	}
	var path *uint16
	if hr, _, _ := procGetAppContainerFolderPath.Call(uintptr(unsafe.Pointer(p)), uintptr(unsafe.Pointer(&path))); hr != 0 {
		return "", errors.New("profile folder unavailable")
	}
	defer windows.CoTaskMemFree(unsafe.Pointer(path))
	return windows.UTF16PtrToString(path), nil
}

func originPath(root, name string) string {
	return filepath.Join(root, "appcontainers", name+".json")
}

func readOrigin(root, name string) (*containerOrigin, error) {
	var origin containerOrigin
	if _, err := readControl(originPath(root, name), &origin); err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, accessCode("profile-origin-unreadable")
	}
	return &origin, nil
}

// ensureContainerProfile makes the plan's profile exist and be ours.
func ensureContainerProfile(api profileAPI, root string, plan containerPlan, want containerOrigin) error {
	want.Protocol, want.Name, want.SID, want.Build = deliveryProtocol, plan.Name, plan.SID.String(), executorSourceIdentity
	dirs, err := managedDirectory(root, "appcontainers")
	defer closeFiles(dirs)
	if err != nil {
		return err
	}
	existing, err := readOrigin(root, plan.Name)
	if err != nil {
		return err
	}
	if existing != nil {
		if existing.Name != want.Name || existing.SID != want.SID || existing.Installation != want.Installation || existing.Host != want.Host ||
			existing.Person != want.Person || existing.Context != want.Context {
			return accessCode("profile-origin-mismatch")
		}
		if existing.State == "collision" {
			return accessCode("profile-collision")
		}
	} else {
		want.State = "creating"
		if err := durableJSON(originPath(root, plan.Name), want); err != nil {
			return err
		}
	}
	sid, exists, err := api.create(plan.Name)
	if err != nil {
		return err
	}
	if exists && (existing == nil) {
		// Present before our record: someone else's profile. Never adopt it.
		want.State = "collision"
		_ = durableJSON(originPath(root, plan.Name), want)
		return accessCode("profile-collision")
	}
	if !exists && !sid.Equals(plan.SID) {
		return accessCode("profile-sid-mismatch")
	}
	if err := verifyProfileFolder(api, plan); err != nil {
		return err
	}
	if existing == nil || existing.State != "created" {
		want.State = "created"
		return durableJSON(originPath(root, plan.Name), want)
	}
	return nil
}

// verifyProfileFolder checks the profile's storage DACL: no ALL APPLICATION
// PACKAGES entry and no other package SID. Jobs never use profile storage.
func verifyProfileFolder(api profileAPI, plan containerPlan) error {
	path, err := api.folder(plan.SID)
	if err != nil {
		return err
	}
	sd, err := windows.GetNamedSecurityInfo(path, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		return accessCode("profile-folder-unreadable")
	}
	return profileDACLAllowed(sd, plan)
}

func profileDACLAllowed(sd *windows.SECURITY_DESCRIPTOR, plan containerPlan) error {
	dacl, _, err := sd.DACL()
	if err != nil || dacl == nil {
		return accessCode("profile-folder-unprotected")
	}
	for i := 0; i < int(dacl.AceCount); i++ {
		var ace *windows.ACCESS_ALLOWED_ACE
		if err := windows.GetAce(dacl, uint32(i), &ace); err != nil {
			return accessCode("profile-folder-unreadable")
		}
		if ace.Header.AceType != windows.ACCESS_ALLOWED_ACE_TYPE {
			continue
		}
		sid := (*windows.SID)(unsafe.Pointer(&ace.SidStart)).String()
		if sid == "S-1-15-2-1" || sid == "S-1-15-2-2" {
			return accessCode("profile-folder-broad")
		}
		if len(sid) > 9 && sid[:9] == "S-1-15-2-" && sid != plan.SID.String() {
			return accessCode("profile-folder-foreign")
		}
	}
	return nil
}

// deleteContainerProfile removes a profile this executor created. The caller
// has verified that no running, issued-only or unknown receipt names it.
func deleteContainerProfile(api profileAPI, root string, plan containerPlan) error {
	existing, err := readOrigin(root, plan.Name)
	if err != nil {
		return err
	}
	if existing == nil || existing.SID != plan.SID.String() || existing.State == "collision" {
		return accessCode("profile-not-owned")
	}
	if err := api.remove(plan.Name); err != nil {
		return err
	}
	return os.Remove(originPath(root, plan.Name))
}
