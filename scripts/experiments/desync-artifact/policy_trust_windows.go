package main

// Operator policy trust (milestone 2, first step). The executor believes the
// operator policy only when the file and every folder above it can be changed
// by trusted principals alone: the named operator pinned in the executor's
// configuration, Administrators, SYSTEM and TrustedInstaller. The executor's
// own service account must be none of them and must hold no right that could
// write, replace, re-own or re-permission the policy or its ancestors. The
// checks run on the same held handles the policy is read through.

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	sidAdministrators   = "S-1-5-32-544"
	sidSystem           = "S-1-5-18"
	sidTrustedInstaller = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
)

// broadSIDs can never be the operator: each names many principals or none.
var broadSIDs = map[string]string{
	"S-1-1-0":      "Everyone",
	"S-1-5-11":     "Authenticated Users",
	"S-1-5-32-545": "Users",
	"S-1-5-32-546": "Guests",
	"S-1-5-4":      "Interactive",
	"S-1-5-2":      "Network",
	"S-1-5-7":      "Anonymous",
	"S-1-5-6":      "Service",
	"S-1-15-2-1":   "All application packages",
	"S-1-15-2-2":   "All restricted application packages",
	"S-1-3-0":      "Creator owner",
	"S-1-3-4":      "Owner rights",
	"S-1-5-32-544": "Administrators (allowed, but not as the named operator)",
	"S-1-5-18":     "SYSTEM (allowed, but not as the named operator)",
}

// Rights that change an object, by the role of that object in the chain.
const (
	fileDeleteChild     = 0x40
	fileAddFile         = 0x2
	fileAddSubdirectory = 0x4
	genericWrite        = 0x40000000
	genericAll          = 0x10000000
	maximumAllowed      = 0x02000000
	replaceRights       = windows.DELETE | windows.WRITE_DAC | windows.WRITE_OWNER | genericAll

	// The policy file: any content, attribute, link source or replacement right.
	policyFileRights = replaceRights | windows.FILE_WRITE_DATA | windows.FILE_APPEND_DATA |
		windows.FILE_WRITE_EA | windows.FILE_WRITE_ATTRIBUTES | genericWrite
	// The folder that holds the policy: no new entries and no child removal.
	policyFolderRights = replaceRights | fileDeleteChild | fileAddFile |
		fileAddSubdirectory | genericWrite
	// Higher folders: nothing that removes, renames, re-owns or re-permissions
	// the chain. Adding unrelated entries beside it is not a replacement.
	ancestorRights = replaceRights | fileDeleteChild
)

type trustRole int

const (
	roleAncestor trustRole = iota
	roleVolumeRoot
	rolePolicyFolder
	rolePolicyFile
)

func (r trustRole) rights() uint32 {
	switch r {
	case rolePolicyFile:
		return policyFileRights
	case rolePolicyFolder:
		return policyFolderRights
	case roleVolumeRoot:
		// A volume root cannot be deleted or renamed.
		return ancestorRights &^ windows.DELETE
	default:
		return ancestorRights
	}
}

func (r trustRole) String() string {
	return [...]string{"ancestor", "volume root", "policy folder", "policy file"}[r]
}

// policyTrust verifies held policy handles. Production uses
// windowsPolicyTrust; nothing else is registered outside tests.
type policyTrust interface {
	verify(chain []*os.File, file *os.File) error
	verifyConfig(chain []*os.File, file *os.File) error
}

type windowsPolicyTrust struct {
	trusted []*windows.SID // operator first
	service windows.Token  // impersonation token of the executor's account
}

// fileMapping is the generic mapping of files and folders.
var fileMapping = struct{ read, write, execute, all uint32 }{0x120089, 0x120116, 0x1200a0, 0x1f01ff}

var (
	advapi32                  = windows.NewLazySystemDLL("advapi32.dll")
	procAccessCheck           = advapi32.NewProc("AccessCheck")
	procCreateRestrictedToken = advapi32.NewProc("CreateRestrictedToken")
)

// sidAccountType resolves what kind of principal a SID names. Production
// asks the local security authority; tests replace it for synthetic SIDs.
var sidAccountType = func(sid *windows.SID) (uint32, error) {
	_, _, use, err := sid.LookupAccount("")
	return use, err
}

// installerSIDs are trusted independently of any configuration: they own the
// bootstrap configuration that names the operator.
func installerSIDs() ([]*windows.SID, error) {
	var out []*windows.SID
	for _, s := range []string{sidAdministrators, sidSystem, sidTrustedInstaller} {
		sid, err := windows.StringToSid(s)
		if err != nil {
			return nil, err
		}
		out = append(out, sid)
	}
	return out, nil
}

// newBootstrapTrust verifies the bootstrap configuration against the fixed
// installer trust only, so a configuration can never bless its own writer by
// naming it operator. The service account must hold none of those identities.
func newBootstrapTrust(service windows.Token) (*windowsPolicyTrust, error) {
	trusted, err := installerSIDs()
	if err != nil {
		return nil, err
	}
	if err := serviceHoldsNone(service, trusted, "service-is-trusted"); err != nil {
		return nil, err
	}
	return &windowsPolicyTrust{trusted, service}, nil
}

// newPolicyTrust adds the named operator, read from a configuration already
// verified by newBootstrapTrust, to the installer trust for the policy. The
// operator must resolve to an individual user account, never a group, alias,
// well-known or capability SID or an unknown SID, and must not be the
// service's own account: the token's user SID is compared whatever its
// attributes, since a restricted or deny-only token is still that account.
func newPolicyTrust(operator string, service windows.Token) (*windowsPolicyTrust, error) {
	sid, err := windows.StringToSid(operator)
	if err != nil || sid.String() != operator {
		return nil, accessCode("operator-invalid")
	}
	if _, broad := broadSIDs[operator]; broad {
		return nil, accessCode("operator-too-broad")
	}
	use, err := sidAccountType(sid)
	if err != nil {
		return nil, accessCode("operator-unresolved")
	}
	if use != windows.SidTypeUser {
		return nil, accessCode("operator-not-a-user")
	}
	if err := serviceHoldsNone(service, []*windows.SID{sid}, "operator-is-service"); err != nil {
		return nil, err
	}
	installer, err := installerSIDs()
	if err != nil {
		return nil, err
	}
	if err := serviceHoldsNone(service, installer, "service-is-trusted"); err != nil {
		return nil, err
	}
	return &windowsPolicyTrust{append([]*windows.SID{sid}, installer...), service}, nil
}

// serviceHoldsNone refuses when the token's user is one of sids, whatever its
// attributes, or when an enabled group is.
func serviceHoldsNone(service windows.Token, sids []*windows.SID, code string) error {
	user, err := service.GetTokenUser()
	if err != nil {
		return err
	}
	groups, err := service.GetTokenGroups()
	if err != nil {
		return err
	}
	for _, t := range sids {
		if user.User.Sid.Equals(t) {
			return accessCode(code)
		}
		for _, g := range groups.AllGroups() {
			if g.Attributes&windows.SE_GROUP_ENABLED != 0 && g.Sid.Equals(t) {
				return accessCode(code)
			}
		}
	}
	return nil
}

// serviceToken returns an identification token of this process for
// AccessCheck. The caller closes it.
func serviceToken() (windows.Token, error) {
	var process windows.Token
	if err := windows.OpenProcessToken(windows.CurrentProcess(), windows.TOKEN_QUERY|windows.TOKEN_DUPLICATE, &process); err != nil {
		return 0, err
	}
	defer process.Close()
	return identificationToken(process)
}

func identificationToken(token windows.Token) (windows.Token, error) {
	var out windows.Token
	err := windows.DuplicateTokenEx(token, windows.TOKEN_QUERY|windows.TOKEN_IMPERSONATE|windows.TOKEN_DUPLICATE, nil,
		windows.SecurityIdentification, windows.TokenImpersonation, &out)
	return out, err
}

func (t *windowsPolicyTrust) isTrusted(sid *windows.SID) bool {
	for _, s := range t.trusted {
		if sid.Equals(s) {
			return true
		}
	}
	return false
}

// verify checks the volume, then every held folder from the volume root down,
// then the file, each through its own handle.
func (t *windowsPolicyTrust) verify(chain []*os.File, file *os.File) error {
	return t.verifyChain(chain, file, rolePolicyFolder)
}

// configReadHook runs between the bootstrap check and the read (tests only).
var configReadHook func()

// loadSharedConfig reads and strictly parses the bootstrap configuration
// through one held handle. Its folders are held without delete sharing; the
// file is opened without following links and without write or delete
// sharing, must be one plain file with one link, within the control size
// bound, at exactly the configured final path. bootstrap then checks the
// volume and every held descriptor; the file's folder may receive the
// executor's own offer, so it is held to the higher-folder rule. Only the
// bytes read through that handle are parsed.
func loadSharedConfig(path string, bootstrap policyTrust, value any) error {
	if bootstrap == nil {
		return accessCode("config-trust-missing")
	}
	if err := localPath(path); err != nil {
		return accessCode("config-not-plain")
	}
	chain, err := holdPolicyChain(filepath.Dir(path))
	defer closeFiles(chain)
	if err != nil {
		return accessCode("config-not-plain")
	}
	f, err := nativeOpen(path, windows.GENERIC_READ, windows.FILE_SHARE_READ, windows.OPEN_EXISTING, 0)
	if os.IsNotExist(err) {
		return accessCode("config-unavailable")
	} else if err != nil {
		// nativeOpen refuses a reparse point or a second hard link.
		return accessCode("config-not-plain")
	}
	defer f.Close()
	var info windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(windows.Handle(f.Fd()), &info); err != nil ||
		info.FileAttributes&(windows.FILE_ATTRIBUTE_REPARSE_POINT|windows.FILE_ATTRIBUTE_DIRECTORY) != 0 || info.NumberOfLinks != 1 {
		return accessCode("config-not-plain")
	}
	if uint64(info.FileSizeHigh)<<32|uint64(info.FileSizeLow) > maxControlBytes {
		return accessCode("config-too-large")
	}
	final, err := finalPath(f)
	if err != nil || !strings.EqualFold(final, filepath.Clean(path)) {
		return accessCode("config-not-plain")
	}
	if err := bootstrap.verifyConfig(chain, f); err != nil {
		return fmt.Errorf("executor configuration: %w", err)
	}
	if configReadHook != nil {
		configReadHook()
	}
	data, err := io.ReadAll(io.LimitReader(f, maxControlBytes+1))
	if err != nil {
		return err
	}
	if len(data) > maxControlBytes {
		return accessCode("config-too-large")
	}
	return strictControlBytes(data, value)
}

func (t *windowsPolicyTrust) verifyConfig(chain []*os.File, file *os.File) error {
	return t.verifyChain(chain, file, roleAncestor)
}

func (t *windowsPolicyTrust) verifyChain(chain []*os.File, file *os.File, folderRole trustRole) error {
	if err := localVolume(file); err != nil {
		return err
	}
	for i, folder := range chain {
		role := roleAncestor
		switch {
		case i == len(chain)-1:
			role = folderRole // also when the volume root holds the file
		case i == 0:
			role = roleVolumeRoot
		}
		if err := t.verifyHandle(folder, role); err != nil {
			return err
		}
	}
	return t.verifyHandle(file, rolePolicyFile)
}

func (t *windowsPolicyTrust) verifyHandle(f *os.File, role trustRole) error {
	sd, err := windows.GetSecurityInfo(windows.Handle(f.Fd()), windows.SE_FILE_OBJECT,
		windows.OWNER_SECURITY_INFORMATION|windows.GROUP_SECURITY_INFORMATION|windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		return accessCode("policy-trust-unreadable")
	}
	return t.verifyDescriptor(sd, role)
}

// verifyDescriptor is the decision on one security descriptor. The owner must
// be trusted (an owner can always rewrite the DACL). Every allow entry that
// applies to the object and carries a changing right for its role must name a
// trusted principal; deny entries are not credited, so a broad allow is
// refused even when a deny narrows it. Finally the effective rights of the
// service token, which include inheritance, deny entries and owner rights,
// must hold none of those rights.
func (t *windowsPolicyTrust) verifyDescriptor(sd *windows.SECURITY_DESCRIPTOR, role trustRole) error {
	owner, _, err := sd.Owner()
	if err != nil || owner == nil {
		return accessCode("policy-trust-unreadable")
	}
	if !t.isTrusted(owner) {
		return trustRefusal("policy-owner-untrusted", role, owner)
	}
	dacl, _, err := sd.DACL()
	if err != nil || dacl == nil {
		// A missing DACL grants everyone everything.
		return trustRefusal("policy-writable", role, nil)
	}
	for i := 0; i < int(dacl.AceCount); i++ {
		var ace *windows.ACCESS_ALLOWED_ACE
		if err := windows.GetAce(dacl, uint32(i), &ace); err != nil {
			return accessCode("policy-trust-unreadable")
		}
		if ace.Header.AceFlags&windows.INHERIT_ONLY_ACE != 0 {
			continue // applies only to children, which are checked themselves
		}
		switch ace.Header.AceType {
		case windows.ACCESS_DENIED_ACE_TYPE, 0x6, 0xa, 0xc: // deny, object, callback, callback object
			continue
		case windows.ACCESS_ALLOWED_ACE_TYPE:
		default:
			return trustRefusal("policy-ace-unsupported", role, nil)
		}
		if uint32(ace.Mask)&role.rights() == 0 {
			continue
		}
		sid := (*windows.SID)(unsafe.Pointer(&ace.SidStart))
		if !t.isTrusted(sid) {
			return trustRefusal("policy-writable", role, sid)
		}
	}
	granted, err := effectiveRights(sd, t.service)
	if err != nil {
		return accessCode("policy-trust-unreadable")
	}
	if granted&role.rights() != 0 {
		return trustRefusal("policy-writable-by-service", role, nil)
	}
	return nil
}

func trustRefusal(code string, role trustRole, sid *windows.SID) error {
	detail := role.String()
	if sid != nil {
		name := sid.String()
		if label, ok := broadSIDs[name]; ok {
			name = label
		}
		detail += " " + name
	}
	return &trustError{code, detail}
}

// trustError reads as its code, so callers compare codes; the detail names the
// object role and principal for the operator's log.
type trustError struct{ code, detail string }

func (e *trustError) Error() string  { return e.code }
func (e *trustError) Detail() string { return e.code + ": " + e.detail }

// effectiveRights asks Windows which rights the token holds on the descriptor.
func effectiveRights(sd *windows.SECURITY_DESCRIPTOR, token windows.Token) (uint32, error) {
	var privileges [64]byte
	size := uint32(len(privileges))
	var granted, status uint32
	mapping := fileMapping
	r, _, err := procAccessCheck.Call(uintptr(unsafe.Pointer(sd)), uintptr(token), maximumAllowed,
		uintptr(unsafe.Pointer(&mapping)), uintptr(unsafe.Pointer(&privileges[0])), uintptr(unsafe.Pointer(&size)),
		uintptr(unsafe.Pointer(&granted)), uintptr(unsafe.Pointer(&status)))
	if r == 0 {
		return 0, err
	}
	if status == 0 {
		return 0, nil
	}
	return granted, nil
}

// localVolume refuses a policy on a remote, removable or ACL-less volume.
func localVolume(f *os.File) error {
	name := make([]uint16, windows.MAX_PATH+1)
	if err := windows.GetVolumeInformationByHandle(windows.Handle(f.Fd()), nil, 0, nil, nil, nil, &name[0], uint32(len(name))); err != nil {
		return accessCode("policy-volume-unsupported")
	}
	final, err := finalPath(f)
	if err != nil {
		return accessCode("policy-volume-unsupported")
	}
	return volumeAllowed(windows.UTF16ToString(name), driveType(final))
}

func driveType(path string) uint32 {
	volume := filepath.VolumeName(path)
	if len(volume) != 2 || volume[1] != ':' {
		return windows.DRIVE_UNKNOWN
	}
	root, err := windows.UTF16PtrFromString(volume + `\`)
	if err != nil {
		return windows.DRIVE_UNKNOWN
	}
	return windows.GetDriveType(root)
}

func volumeAllowed(fileSystem string, drive uint32) error {
	if drive != windows.DRIVE_FIXED {
		return accessCode("policy-volume-unsupported")
	}
	switch strings.ToUpper(fileSystem) {
	case "NTFS", "REFS":
		return nil
	}
	return accessCode("policy-volume-unsupported")
}

// holdPolicyChain opens the volume root and every folder down to dir without
// following links and without delete sharing, so none of them can be renamed
// or removed while held, with READ_CONTROL for the trust checks.
func holdPolicyChain(dir string) ([]*os.File, error) {
	var names []string
	for p := filepath.Clean(dir); ; p = filepath.Dir(p) {
		names = append(names, p)
		if p == filepath.Dir(p) {
			break
		}
	}
	var files []*os.File
	for i := len(names) - 1; i >= 0; i-- {
		f, err := nativeOpen(names[i], windows.FILE_READ_ATTRIBUTES|windows.READ_CONTROL,
			windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS)
		if err != nil {
			return files, err
		}
		files = append(files, f)
		var info windows.ByHandleFileInformation
		if err := windows.GetFileInformationByHandle(windows.Handle(f.Fd()), &info); err != nil {
			return files, err
		}
		if info.FileAttributes&windows.FILE_ATTRIBUTE_DIRECTORY == 0 || info.FileAttributes&windows.FILE_ATTRIBUTE_REPARSE_POINT != 0 {
			return files, fmt.Errorf("%s is not a plain folder", filepath.Base(names[i]))
		}
	}
	return files, nil
}

// restrictedToken builds a token that holds only what both base and the
// restricting SIDs are granted, with disable SIDs turned deny-only. Tests use
// it as a synthetic service account distinct from the test's own user.
func restrictedToken(base windows.Token, disable, restrict []*windows.SID) (windows.Token, error) {
	pack := func(sids []*windows.SID) (uintptr, uintptr) {
		if len(sids) == 0 {
			return 0, 0
		}
		entries := make([]windows.SIDAndAttributes, len(sids))
		for i, s := range sids {
			entries[i].Sid = s
		}
		return uintptr(len(entries)), uintptr(unsafe.Pointer(&entries[0]))
	}
	disableCount, disablePtr := pack(disable)
	restrictCount, restrictPtr := pack(restrict)
	var out windows.Token
	r, _, err := procCreateRestrictedToken.Call(uintptr(base), 0, disableCount, disablePtr, 0, 0, restrictCount, restrictPtr, uintptr(unsafe.Pointer(&out)))
	if r == 0 {
		return 0, err
	}
	return out, nil
}

var errNoPolicyTrust = errors.New("policy-trust-missing")

// trustDetail expands a trust refusal with its object role and principal for
// the operator's start log; other errors pass unchanged.
func trustDetail(err error) error {
	var refusal *trustError
	if errors.As(err, &refusal) {
		return errors.New(strings.Replace(err.Error(), refusal.code, refusal.Detail(), 1))
	}
	return err
}

// sharedBootstrap verifies the bootstrap configuration of the multi-person
// entry. Production uses the fixed installer trust; tests replace it to reach
// later refusals.
var sharedBootstrap = func(service windows.Token) (policyTrust, error) { return newBootstrapTrust(service) }

// serveSharedExecutor is the multi-person service entry, the only one that
// starts that mode. The configuration is read through one held, checked
// handle and verified against the fixed installer trust (Administrators,
// SYSTEM, TrustedInstaller) before any field in it is used; only then does its
// named operator become trusted for the policy. A configuration without an
// access section is refused, never served as a single-person executor.
func serveSharedExecutor(configPath string) error {
	token, err := serviceToken()
	if err != nil {
		return err
	}
	defer token.Close()
	bootstrap, err := sharedBootstrap(token)
	if err != nil {
		return errors.New("executor service account: " + err.Error())
	}
	var cfg ExecutorConfig
	if err := loadSharedConfig(configPath, bootstrap, &cfg); err != nil {
		return trustDetail(err)
	}
	if cfg.Access == nil {
		return errors.New("executor-serve-shared needs an access section")
	}
	if cfg.Access.Isolation == appContainerBackend {
		if err := activateAppContainer(bootstrap, cfg.Root); err != nil {
			return errors.New("isolation backend unavailable: " + err.Error())
		}
	}
	if err := validateAccessConfig(cfg); err != nil {
		return err
	}
	trust, err := newPolicyTrust(cfg.Access.Operator, token)
	if err != nil {
		return errors.New("access operator: " + err.Error())
	}
	return runExecutor(cfg, configPath, trust)
}
