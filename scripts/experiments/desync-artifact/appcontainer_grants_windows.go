package main

// Applying the grant matrix (appcontainer_acl.go) to a job's folders and a
// project's inputs, and reading every grant back through the same handle.
// Only the shared executor with a registered AppContainer backend and the
// disposable-runner qualification call these; nothing here runs on a
// developer computer.

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"
)

var procTreeSetNamedSecurityInfo = windows.NewLazySystemDLL("advapi32.dll").NewProc("TreeSetNamedSecurityInfoW")

// containedLayout names the folders of one contained job.
type containedLayout struct {
	Root                     string // the executor root
	Parent, Runtime, Outputs string // jobs/<job>, its runtime and outputs
	Generation, Bundle       string // the project's leased input and scenario bundle
}

func serviceSID() (*windows.SID, error) {
	var token windows.Token
	if err := windows.OpenProcessToken(windows.CurrentProcess(), windows.TOKEN_QUERY, &token); err != nil {
		return nil, err
	}
	defer token.Close()
	user, err := token.GetTokenUser()
	if err != nil {
		return nil, err
	}
	return user.User.Sid.Copy()
}

func grantFor(role grantRole, principal grantPrincipal) (containedGrant, error) {
	for _, g := range appContainerGrants {
		if g.Role == role && g.Principal == principal {
			return g, nil
		}
	}
	return containedGrant{}, fmt.Errorf("no %s grant for %s", principal, role)
}

// privateFolderSDDL is the complete protected descriptor of a job-private
// writable folder: the executor's account and SYSTEM, the job's package SID
// per the matrix, and a Low no-write-up label.
func privateFolderSDDL(service *windows.SID, g containedGrant, plan containerPlan) (string, error) {
	dacl, label, err := grantSDDL(g, plan)
	if err != nil {
		return "", err
	}
	if label == "" {
		return "", errors.New("writable contained folders need a Low label")
	}
	return "D:P(A;OICI;FA;;;" + service.String() + ")(A;OICI;FA;;;SY)" + dacl + "S:" + label, nil
}

// applyPrivateFolder sets a protected descriptor on an empty, plain folder
// through one handle and reads it back.
func applyPrivateFolder(path, sddl string) error {
	f, err := nativeOpen(path, windows.READ_CONTROL|windows.WRITE_DAC|windows.WRITE_OWNER|windows.FILE_LIST_DIRECTORY,
		windows.FILE_SHARE_READ, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS)
	if err != nil {
		return accessCode("container-folder-unavailable")
	}
	defer f.Close()
	if entries, err := f.Readdirnames(1); err == nil && len(entries) != 0 {
		return accessCode("container-folder-not-empty")
	}
	sd, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		return err
	}
	dacl, _, _ := sd.DACL()
	sacl, _, _ := sd.SACL()
	info := windows.SECURITY_INFORMATION(windows.DACL_SECURITY_INFORMATION | windows.PROTECTED_DACL_SECURITY_INFORMATION | windows.LABEL_SECURITY_INFORMATION)
	if err := windows.SetSecurityInfo(windows.Handle(f.Fd()), windows.SE_FILE_OBJECT, info, nil, nil, dacl, sacl); err != nil {
		return accessCode("container-grant-failed")
	}
	return readBackDescriptor(windows.Handle(f.Fd()), sd)
}

// readBackDescriptor compares the DACL and label read through the handle
// with what was set.
func readBackDescriptor(h windows.Handle, want *windows.SECURITY_DESCRIPTOR) error {
	got, err := windows.GetSecurityInfo(h, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION|windows.LABEL_SECURITY_INFORMATION)
	if err != nil {
		return accessCode("container-grant-unreadable")
	}
	if !sameProtection(got, want) {
		return accessCode("container-grant-mismatch")
	}
	return nil
}

// aceEntries lists an ACL's entries as "type/flags/mask/sid" with the
// inherited flag removed, sorted; the system's own normalization (entry
// order, the auto-inherited control flag, alias spelling) does not matter,
// the entries do.
func aceEntries(acl *windows.ACL) ([]string, error) {
	if acl == nil {
		return nil, nil
	}
	var out []string
	for i := 0; i < int(acl.AceCount); i++ {
		var ace *windows.ACCESS_ALLOWED_ACE
		if err := windows.GetAce(acl, uint32(i), &ace); err != nil {
			return nil, err
		}
		sid := (*windows.SID)(unsafe.Pointer(&ace.SidStart))
		flags := ace.Header.AceFlags &^ windows.INHERITED_ACE
		out = append(out, fmt.Sprintf("%d/%d/%#x/%s", ace.Header.AceType, flags, uint32(ace.Mask), sid.String()))
	}
	sort.Strings(out)
	return out, nil
}

// sameProtection compares two descriptors by meaning: the DACL's protection,
// its entries and the mandatory label entries.
func sameProtection(got, want *windows.SECURITY_DESCRIPTOR) bool {
	gotControl, _, err1 := got.Control()
	wantControl, _, err2 := want.Control()
	if err1 != nil || err2 != nil || gotControl&windows.SE_DACL_PROTECTED != wantControl&windows.SE_DACL_PROTECTED {
		return false
	}
	gotDACL, _, err1 := got.DACL()
	wantDACL, _, err2 := want.DACL()
	if err1 != nil || err2 != nil {
		return false
	}
	a, err1 := aceEntries(gotDACL)
	b, err2 := aceEntries(wantDACL)
	if err1 != nil || err2 != nil || strings.Join(a, ",") != strings.Join(b, ",") {
		return false
	}
	gotSACL, _, _ := got.SACL()
	wantSACL, _, _ := want.SACL()
	a, err1 = aceEntries(gotSACL)
	b, err2 = aceEntries(wantSACL)
	return err1 == nil && err2 == nil && strings.Join(a, ",") == strings.Join(b, ",")
}

func descriptorParts(sd *windows.SECURITY_DESCRIPTOR) string {
	text := sd.String()
	if i := strings.Index(text, "D:"); i >= 0 {
		text = text[i:]
	}
	return text
}

// addTraverse adds the job package's non-inherited traverse entry to the job
// parent, keeping its inherited entries, and reads it back.
func addTraverse(path string, plan containerPlan) error {
	g, err := grantFor(roleJobParent, principalPackage)
	if err != nil {
		return err
	}
	return addExplicitEntry(path, g, plan, false)
}

// pathFolders lists the folders strictly between the volume root and path.
func pathFolders(path string) []string {
	var out []string
	for p := filepath.Dir(filepath.Clean(path)); p != filepath.Dir(p); p = filepath.Dir(p) {
		out = append([]string{p}, out...)
	}
	return out
}

// grantPathListing lets the job's containers list (names only) every folder
// on the way to their runtime, generation and bundle: cmd.exe opens and
// lists each ancestor before running a batch file. The executor root's
// ancestors, the root and its jobs folder take the installation's jobs
// capability (an entry the operator may also provision ahead); the folders
// below the root on the way to a project's inputs take that project's
// capability. Nothing is inherited, so no file below grants anything new.
func grantPathListing(layout containedLayout, plan containerPlan) error {
	if layout.Root == "" {
		return accessCode("container-layout-invalid")
	}
	jobs, err := grantFor(roleExecutorPath, principalJobs)
	if err != nil {
		return err
	}
	project, err := grantFor(roleProjectPath, principalProject)
	if err != nil {
		return err
	}
	root := filepath.Clean(layout.Root)
	if err := volumeRootListed(root, plan); err != nil {
		return err
	}
	for _, dir := range append(pathFolders(root), root, filepath.Join(root, "jobs")) {
		if err := addExplicitEntry(dir, jobs, plan, false); err != nil {
			return fmt.Errorf("%w (%s)", accessCode("container-path-unlisted"), dir)
		}
	}
	for _, target := range []string{layout.Generation, layout.Bundle} {
		for _, dir := range pathFolders(target) {
			if len(dir) <= len(root) || !strings.HasPrefix(strings.ToLower(dir), strings.ToLower(root)+`\`) {
				continue
			}
			if err := addExplicitEntry(dir, project, plan, false); err != nil {
				return fmt.Errorf("%w (%s)", accessCode("container-path-unlisted"), dir)
			}
		}
	}
	return nil
}

// volumeRootListed checks that the volume root of the executor root lets the
// installation's jobs capability list folder names: cmd.exe lists it before
// running any batch file. Only the operator can grant that (the command
// `hgs-executor appcontainer-capability` prints it); the executor never
// changes a volume root.
func volumeRootListed(root string, plan containerPlan) error {
	var jobs *windows.SID
	for i, name := range plan.CapabilityNames {
		if strings.HasPrefix(name, "zerusJobs") && i < len(plan.Capabilities) {
			jobs = plan.Capabilities[i]
		}
	}
	if jobs == nil {
		return accessCode("container-plan-invalid")
	}
	volume := filepath.VolumeName(root) + `\`
	sd, err := windows.GetNamedSecurityInfo(volume, windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		return accessCode("container-volume-root-unreadable")
	}
	dacl, _, err := sd.DACL()
	if err != nil || dacl == nil {
		return accessCode("container-volume-root-unlisted")
	}
	need := uint32(windows.FILE_LIST_DIRECTORY | windows.FILE_TRAVERSE)
	for i := 0; i < int(dacl.AceCount); i++ {
		var ace *windows.ACCESS_ALLOWED_ACE
		if windows.GetAce(dacl, uint32(i), &ace) != nil || ace.Header.AceType != windows.ACCESS_ALLOWED_ACE_TYPE {
			continue
		}
		sid := (*windows.SID)(unsafe.Pointer(&ace.SidStart))
		if (sid.Equals(jobs) || sid.String() == "S-1-15-2-1") && uint32(ace.Mask)&need == need {
			return nil
		}
	}
	return fmt.Errorf("%w: the operator must let %s list %s (hgs-executor appcontainer-capability prints the command)", accessCode("container-volume-root-unlisted"), jobs, volume)
}

// ensureProjectGrant gives the project capability inherited read and execute
// on an input generation or scenario bundle, once: an existing exact entry is
// kept. Existing files receive the inherited entry through the tree walk;
// files installed later inherit it at creation. These folders live in the
// executor root, which no contained principal can write.
func ensureProjectGrant(path string, role grantRole, plan containerPlan) error {
	g, err := grantFor(role, principalProject)
	if err != nil {
		return err
	}
	return addExplicitEntry(path, g, plan, true)
}

func addExplicitEntry(path string, g containedGrant, plan containerPlan, tree bool) error {
	f, err := nativeOpen(path, windows.READ_CONTROL|windows.WRITE_DAC, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS)
	if err != nil {
		return accessCode("container-folder-unavailable")
	}
	defer f.Close()
	current, err := windows.GetSecurityInfo(windows.Handle(f.Fd()), windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		return accessCode("container-grant-unreadable")
	}
	entry, _, err := grantSDDL(g, plan)
	if err != nil {
		return err
	}
	explicit, already := explicitEntries(current, entry)
	if already {
		return nil
	}
	sd, err := windows.SecurityDescriptorFromString("D:" + explicit + entry)
	if err != nil {
		return err
	}
	dacl, _, _ := sd.DACL()
	info := windows.SECURITY_INFORMATION(windows.DACL_SECURITY_INFORMATION | windows.UNPROTECTED_DACL_SECURITY_INFORMATION)
	if tree {
		p, _ := windows.UTF16PtrFromString(path)
		if r, _, _ := procTreeSetNamedSecurityInfo.Call(uintptr(unsafe.Pointer(p)), uintptr(windows.SE_FILE_OBJECT), uintptr(info), 0, 0,
			uintptr(unsafe.Pointer(dacl)), 0, 2 /* TREE_SEC_INFO_SET */, 0, 0, 0); r != 0 {
			return accessCode("container-grant-failed")
		}
	} else if err := windows.SetSecurityInfo(windows.Handle(f.Fd()), windows.SE_FILE_OBJECT, info, nil, nil, dacl, nil); err != nil {
		return accessCode("container-grant-failed")
	}
	after, err := windows.GetSecurityInfo(windows.Handle(f.Fd()), windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		return accessCode("container-grant-unreadable")
	}
	if _, present := explicitEntries(after, entry); !present {
		return accessCode("container-grant-mismatch")
	}
	return nil
}

// explicitEntries returns the descriptor's non-inherited DACL entries as SDDL
// and whether entry is among them.
func explicitEntries(sd *windows.SECURITY_DESCRIPTOR, entry string) (string, bool) {
	text := descriptorParts(sd)
	text = strings.TrimPrefix(text, "D:")
	for len(text) > 0 && text[0] != '(' {
		text = text[1:] // flags such as P, AI
	}
	var explicit strings.Builder
	found := false
	for _, part := range strings.SplitAfter(text, ")") {
		if !strings.HasPrefix(part, "(") {
			continue
		}
		fields := strings.Split(strings.Trim(part, "()"), ";")
		if len(fields) > 1 && strings.Contains(fields[1], "ID") {
			continue
		}
		if strings.EqualFold(part, entry) {
			found = true
		}
		explicit.WriteString(part)
	}
	return explicit.String(), found
}

// prepareContainedJob applies the whole matrix for one job: its private
// folders, the parent's traverse entry and the project's input grants.
func prepareContainedJob(layout containedLayout, plan containerPlan) error {
	if err := validateGrantMatrix(appContainerGrants); err != nil {
		return err
	}
	service, err := serviceSID()
	if err != nil {
		return err
	}
	for _, folder := range []struct {
		path string
		role grantRole
	}{{layout.Runtime, roleJobHome}, {layout.Outputs, roleJobOut}} {
		if _, err := os.Stat(folder.path); err != nil {
			return accessCode("container-folder-unavailable")
		}
		g, err := grantFor(folder.role, principalPackage)
		if err != nil {
			return err
		}
		sddl, err := privateFolderSDDL(service, g, plan)
		if err != nil {
			return err
		}
		if err := applyPrivateFolder(folder.path, sddl); err != nil {
			return err
		}
	}
	if err := addTraverse(layout.Parent, plan); err != nil {
		return err
	}
	if err := grantPathListing(layout, plan); err != nil {
		return err
	}
	if err := ensureProjectGrant(layout.Generation, roleGeneration, plan); err != nil {
		return err
	}
	return ensureProjectGrant(layout.Bundle, roleBundle, plan)
}
