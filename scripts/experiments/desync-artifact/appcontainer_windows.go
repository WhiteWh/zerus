package main

// Candidate isolation backend "windows-appcontainer-v1" (milestone 2): the
// launch plan, its pure derivations and the token verification that must pass
// before a contained process is resumed. Nothing here is registered in
// isolationBackends, creates an AppContainer profile, changes an ACL or starts
// a process; nativeScenario.Start is unchanged. The qualification harness
// (appcontainer_qualification_test.go, disposable runner only) is the only
// caller that launches contained processes.

import (
	"crypto/sha256"
	"encoding/hex"
	"sort"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	appContainerBackend  = "windows-appcontainer-v1"
	appContainerContract = "zerus-appcontainer-v1"

	// Token information classes not named by x/sys/windows.
	tokenIsAppContainer  = 29
	tokenCapabilities    = 30
	tokenAppContainerSid = 31

	// Process attributes for CreateProcess (winbase.h).
	procThreadAttributeSecurityCapabilities = 0x00020009
	procThreadAttributeAllApplicationPolicy = 0x0002000f // LPAC opt-out of ALL APPLICATION PACKAGES
	sidLowIntegrity                         = "S-1-16-4096"
)

var (
	userenv                          = windows.NewLazySystemDLL("userenv.dll")
	kernelbase                       = windows.NewLazySystemDLL("kernelbase.dll")
	procDeriveAppContainerSid        = userenv.NewProc("DeriveAppContainerSidFromAppContainerName")
	procDeriveCapabilitySidsFromName = kernelbase.NewProc("DeriveCapabilitySidsFromName")
	procFreeSid                      = windows.NewLazySystemDLL("advapi32.dll").NewProc("FreeSid")
)

// securityCapabilities is SECURITY_CAPABILITIES (winnt.h).
type securityCapabilities struct {
	AppContainerSid *windows.SID
	Capabilities    *windows.SIDAndAttributes
	CapabilityCount uint32
	Reserved        uint32
}

// containerPlan is everything a contained launch needs, derived only from the
// executor's own identities and a freshly verified policy: never from a
// request, a stored SID or an existing profile.
type containerPlan struct {
	Name            string // AppContainer profile name, at most 64 characters
	SID             *windows.SID
	CapabilityNames []string // sorted; each derived by DeriveCapabilitySidsFromName
	Capabilities    []*windows.SID
	LessPrivileged  bool // LPAC; not enabled in this phase
}

func isHex64(s string) bool {
	if len(s) != 64 {
		return false
	}
	_, err := hex.DecodeString(s)
	return err == nil && strings.ToLower(s) == s
}

func derivedLabel(parts ...string) string {
	h := sha256.Sum256([]byte(appContainerContract + "\n" + strings.Join(parts, "\n") + "\n"))
	return hex.EncodeToString(h[:])
}

// containerName names the container of one person's context on one executor
// installation and host. installation is the executor root's identity and
// host the executor's TLS identity, both 64 lowercase hex characters, so two
// executors on one computer, or a rebuilt root, never share a container.
func containerName(installation, host, person, context string) (string, error) {
	if !isHex64(installation) || !isHex64(host) || !isHex64(person) || !accessName(context) {
		return "", accessCode("container-identity-invalid")
	}
	return "Zerus.AC." + derivedLabel("container", installation, host, person, context)[:40], nil
}

// projectCapabilityName names the capability that reads one project's input
// generation and scenario bundle on one installation.
func projectCapabilityName(installation, project string) (string, error) {
	if !isHex64(installation) || !accessName(project) {
		return "", accessCode("capability-identity-invalid")
	}
	return "zerusProject" + derivedLabel("project", installation, project)[:32], nil
}

// appContainerSID derives the package SID of a name. The derivation is a pure
// hash: it creates no profile and touches no registry or file.
func appContainerSID(name string) (*windows.SID, error) {
	p, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return nil, err
	}
	var sid *windows.SID
	if r, _, _ := procDeriveAppContainerSid.Call(uintptr(unsafe.Pointer(p)), uintptr(unsafe.Pointer(&sid))); r != 0 {
		return nil, accessCode("container-sid-unavailable")
	}
	defer procFreeSid.Call(uintptr(unsafe.Pointer(sid)))
	return sid.Copy()
}

// capabilitySID derives the capability SID of a capability name (pure).
func capabilitySID(name string) (*windows.SID, error) {
	p, err := windows.UTF16PtrFromString(name)
	if err != nil {
		return nil, err
	}
	var groups, capabilities **windows.SID
	var groupCount, capabilityCount uint32
	r, _, _ := procDeriveCapabilitySidsFromName.Call(uintptr(unsafe.Pointer(p)),
		uintptr(unsafe.Pointer(&groups)), uintptr(unsafe.Pointer(&groupCount)),
		uintptr(unsafe.Pointer(&capabilities)), uintptr(unsafe.Pointer(&capabilityCount)))
	if r == 0 {
		return nil, accessCode("capability-sid-unavailable")
	}
	free := func(list **windows.SID, count uint32) {
		if list == nil {
			return
		}
		for _, s := range unsafe.Slice(list, count) {
			windows.LocalFree(windows.Handle(unsafe.Pointer(s)))
		}
		windows.LocalFree(windows.Handle(unsafe.Pointer(list)))
	}
	defer free(groups, groupCount)
	defer free(capabilities, capabilityCount)
	if capabilityCount != 1 {
		return nil, accessCode("capability-sid-unavailable")
	}
	return unsafe.Slice(capabilities, capabilityCount)[0].Copy()
}

// networkGrant is the executor-local, operator-configured network capability
// of one exact project, backend and job. M2 grants none.
type networkGrant struct{ Capabilities []string }

// planContainer builds the launch plan of an admitted job from the current,
// freshly verified policy. The caller has just authorized jobs.submit for
// owner on owner.Project against the same policy (authorizeExecution); this
// function adds no authority of its own.
func planContainer(installation, host string, owner JobOwner, policy *AccessPolicy, network networkGrant) (containerPlan, error) {
	if policy == nil {
		return containerPlan{}, accessCode("policy-unavailable")
	}
	context, ok := policy.Contexts[owner.Person]
	if !ok {
		return containerPlan{}, accessCode("context-missing")
	}
	if context.Backend != appContainerBackend {
		return containerPlan{}, accessCode("context-backend")
	}
	if len(network.Capabilities) != 0 {
		return containerPlan{}, accessCode("network-not-supported")
	}
	name, err := containerName(installation, host, owner.Person, context.Context)
	if err != nil {
		return containerPlan{}, err
	}
	sid, err := appContainerSID(name)
	if err != nil {
		return containerPlan{}, err
	}
	capabilityName, err := projectCapabilityName(installation, owner.Project)
	if err != nil {
		return containerPlan{}, err
	}
	capability, err := capabilitySID(capabilityName)
	if err != nil {
		return containerPlan{}, err
	}
	return containerPlan{Name: name, SID: sid, CapabilityNames: []string{capabilityName}, Capabilities: []*windows.SID{capability}}, nil
}

// securityCapabilitiesFor is the SECURITY_CAPABILITIES attribute value of a
// plan. The returned slice keeps the entries alive while the attribute list
// is in use.
func securityCapabilitiesFor(plan containerPlan) (*securityCapabilities, []windows.SIDAndAttributes) {
	entries := make([]windows.SIDAndAttributes, len(plan.Capabilities))
	for i, sid := range plan.Capabilities {
		entries[i] = windows.SIDAndAttributes{Sid: sid, Attributes: windows.SE_GROUP_ENABLED}
	}
	caps := &securityCapabilities{AppContainerSid: plan.SID, CapabilityCount: uint32(len(entries))}
	if len(entries) != 0 {
		caps.Capabilities = &entries[0]
	}
	return caps, entries
}

// Privileges a contained token may hold at all; only SeChangeNotify may be
// enabled.
var containedPrivileges = map[string]bool{
	"SeChangeNotifyPrivilege": true, "SeIncreaseWorkingSetPrivilege": true, "SeShutdownPrivilege": true,
	"SeUndockPrivilege": true, "SeTimeZonePrivilege": true,
}

func tokenInformation(token windows.Token, class uint32) ([]byte, error) {
	var size uint32
	_ = windows.GetTokenInformation(token, class, nil, 0, &size)
	if size == 0 || size > 64*1024 {
		return nil, accessCode("token-unreadable")
	}
	buffer := make([]byte, size)
	if err := windows.GetTokenInformation(token, class, &buffer[0], size, &size); err != nil {
		return nil, accessCode("token-unreadable")
	}
	return buffer, nil
}

// verifyContainedProcess is the check between creation and resume: the
// suspended process is in the expected Job and its primary token is exactly
// the planned AppContainer token. Any failure refuses; there is no fallback
// to an uncontained launch.
func verifyContainedProcess(process, job windows.Handle, plan containerPlan) error {
	belongs, err := processInJob(process, job)
	if err != nil || !belongs {
		return accessCode("container-job-unverified")
	}
	var token windows.Token
	if err := windows.OpenProcessToken(process, windows.TOKEN_QUERY, &token); err != nil {
		return accessCode("token-unreadable")
	}
	defer token.Close()
	return verifyContainedToken(token, plan)
}

func verifyContainedToken(token windows.Token, plan containerPlan) error {
	if plan.SID == nil {
		return accessCode("container-plan-invalid")
	}
	if plan.LessPrivileged {
		// The LPAC attribute check is not implemented: fail closed.
		return accessCode("lpac-unverified")
	}
	flag, err := tokenInformation(token, tokenIsAppContainer)
	if err != nil || len(flag) < 4 || *(*uint32)(unsafe.Pointer(&flag[0])) != 1 {
		return accessCode("not-appcontainer")
	}
	info, err := tokenInformation(token, tokenAppContainerSid)
	if err != nil {
		return err
	}
	package_ := *(**windows.SID)(unsafe.Pointer(&info[0]))
	if package_ == nil || !package_.Equals(plan.SID) {
		return accessCode("container-sid-mismatch")
	}
	capInfo, err := tokenInformation(token, tokenCapabilities)
	if err != nil {
		return err
	}
	groups := (*windows.Tokengroups)(unsafe.Pointer(&capInfo[0])).AllGroups()
	var got, want []string
	for _, g := range groups {
		if g.Attributes&windows.SE_GROUP_ENABLED == 0 {
			continue
		}
		got = append(got, g.Sid.String())
	}
	for _, s := range plan.Capabilities {
		want = append(want, s.String())
	}
	sort.Strings(got)
	sort.Strings(want)
	if strings.Join(got, ",") != strings.Join(want, ",") || len(groups) != len(want) {
		return accessCode("container-capabilities-mismatch")
	}
	label, err := tokenInformation(token, windows.TokenIntegrityLevel)
	if err != nil {
		return err
	}
	if (*windows.Tokenmandatorylabel)(unsafe.Pointer(&label[0])).Label.Sid.String() != sidLowIntegrity {
		return accessCode("container-integrity-mismatch")
	}
	privileges, err := tokenInformation(token, windows.TokenPrivileges)
	if err != nil {
		return err
	}
	tp := (*windows.Tokenprivileges)(unsafe.Pointer(&privileges[0]))
	for _, p := range tp.AllPrivileges() {
		name, err := privilegeName(p.Luid)
		if err != nil || !containedPrivileges[name] {
			return accessCode("container-privilege-unexpected")
		}
		if p.Attributes&windows.SE_PRIVILEGE_ENABLED != 0 && name != "SeChangeNotifyPrivilege" {
			return accessCode("container-privilege-enabled")
		}
	}
	return nil
}

var procLookupPrivilegeName = windows.NewLazySystemDLL("advapi32.dll").NewProc("LookupPrivilegeNameW")

func privilegeName(luid windows.LUID) (string, error) {
	buffer := make([]uint16, 128)
	size := uint32(len(buffer))
	if r, _, err := procLookupPrivilegeName.Call(0, uintptr(unsafe.Pointer(&luid)), uintptr(unsafe.Pointer(&buffer[0])), uintptr(unsafe.Pointer(&size))); r == 0 {
		return "", err
	}
	return windows.UTF16ToString(buffer[:size]), nil
}
