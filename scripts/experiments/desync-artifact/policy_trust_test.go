package main

// Milestone 2, first step: operator policy trust (Q8). Every security
// descriptor here is built in memory from SDDL; no ACL, owner, user or
// firewall rule on this computer is created or changed. The real-file tests
// only read the existing descriptors of the test's own temporary folders.
// Fixtures with real changed ACLs belong to the disposable qualification
// runner.

import (
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"golang.org/x/sys/windows"
)

// A synthetic operator SID for configuration fixtures; it names no account.
const syntheticOperator = "S-1-5-21-1000000001-1000000002-1000000003-4101"

// The synthetic service account of the descriptor tests.
const syntheticService = "S-1-5-21-1000000001-1000000002-1000000003-4102"

// acceptTrust stands in for owner and DACL checks in tests of other rules. It
// exists only in tests; production has no accepting trust.
type acceptTrust struct{}

func (acceptTrust) verify([]*os.File, *os.File) error       { return nil }
func (acceptTrust) verifyConfig([]*os.File, *os.File) error { return nil }

// Synthetic principals resolved through the sidAccountType seam.
const (
	syntheticGroup   = "S-1-5-21-1000000001-1000000002-1000000003-4103"
	syntheticUnknown = "S-1-5-21-1000000001-1000000002-1000000003-4104"
)

// syntheticAccountTypes resolves the synthetic operator as a user and the
// synthetic group as a group; the unknown SID and every real SID go to real.
func syntheticAccountTypes(real func(*windows.SID) (uint32, error)) func(*windows.SID) (uint32, error) {
	return func(sid *windows.SID) (uint32, error) {
		switch sid.String() {
		case syntheticOperator:
			return windows.SidTypeUser, nil
		case syntheticGroup:
			return windows.SidTypeGroup, nil
		case syntheticUnknown:
			return 0, errors.New("unmapped")
		}
		return real(sid)
	}
}

func currentUserSID(t *testing.T) *windows.SID {
	t.Helper()
	var token windows.Token
	if err := windows.OpenProcessToken(windows.CurrentProcess(), windows.TOKEN_QUERY, &token); err != nil {
		t.Fatal(err)
	}
	defer token.Close()
	user, err := token.GetTokenUser()
	if err != nil {
		t.Fatal(err)
	}
	sid, err := user.User.Sid.Copy()
	if err != nil {
		t.Fatal(err)
	}
	return sid
}

// syntheticServiceToken is this process's token restricted to the synthetic
// service SID, with the test user and Administrators deny-only: an account
// distinct from the operator (the test user) whose effective rights Windows
// computes from both its groups and the restricting SID.
func syntheticServiceToken(t *testing.T) windows.Token {
	t.Helper()
	var process windows.Token
	if err := windows.OpenProcessToken(windows.CurrentProcess(), windows.TOKEN_QUERY|windows.TOKEN_DUPLICATE|windows.TOKEN_ASSIGN_PRIMARY, &process); err != nil {
		t.Fatal(err)
	}
	defer process.Close()
	service, _ := windows.StringToSid(syntheticService)
	admins, _ := windows.StringToSid(sidAdministrators)
	restricted, err := restrictedToken(process, []*windows.SID{currentUserSID(t), admins}, []*windows.SID{service})
	if err != nil {
		t.Fatal(err)
	}
	defer restricted.Close()
	token, err := identificationToken(restricted)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { token.Close() })
	return token
}

// descriptorTrust is a test-only trust for descriptor decisions: the test
// user stands in as operator beside the installer SIDs, with the synthetic
// service token for AccessCheck. The production constructor refuses this
// pairing (the restricted token is still the test user's account), so it is
// built directly and never through newPolicyTrust.
func descriptorTrust(t *testing.T) *windowsPolicyTrust {
	t.Helper()
	installer, err := installerSIDs()
	if err != nil {
		t.Fatal(err)
	}
	return &windowsPolicyTrust{append([]*windows.SID{currentUserSID(t)}, installer...), syntheticServiceToken(t)}
}

// The operator is pinned, named and distinct from the executor's account.
func TestTheOperatorIsPinnedAndDistinct(t *testing.T) {
	process, err := serviceToken()
	if err != nil {
		t.Fatal(err)
	}
	defer process.Close()
	me := currentUserSID(t).String()
	if _, err := newPolicyTrust(me, process); trustCode(err) != "operator-is-service" {
		t.Fatalf("the executor's own account as operator: %v", err)
	}
	// A restricted token whose user is deny-only is still the same account.
	if _, err := newPolicyTrust(me, syntheticServiceToken(t)); trustCode(err) != "operator-is-service" {
		t.Fatalf("the own account behind a restricted token: %v", err)
	}
	restore := sidAccountType
	t.Cleanup(func() { sidAccountType = restore })
	sidAccountType = syntheticAccountTypes(restore)
	if _, err := newPolicyTrust(syntheticGroup, process); trustCode(err) != "operator-not-a-user" {
		t.Fatalf("a group as operator: %v", err)
	}
	if _, err := newPolicyTrust(syntheticUnknown, process); trustCode(err) != "operator-unresolved" {
		t.Fatalf("an unresolved SID as operator: %v", err)
	}
	for _, broad := range []string{"S-1-1-0", "S-1-5-11", "S-1-5-32-545", "S-1-15-2-1", "S-1-5-32-544", "S-1-5-18", "S-1-3-4"} {
		if _, err := newPolicyTrust(broad, process); trustCode(err) != "operator-too-broad" {
			t.Fatalf("%s as operator: %v", broad, err)
		}
	}
	for _, bad := range []string{"", "alice", "S-1-5-21-x", "s-1-5-21-1-2-3-4"} {
		if _, err := newPolicyTrust(bad, process); trustCode(err) != "operator-invalid" {
			t.Fatalf("%q as operator: %v", bad, err)
		}
	}
	// A distinct named operator is accepted unless this process itself holds
	// administrator rights, which would give the service policy authority.
	elevated := process.IsElevated()
	_, err = newPolicyTrust(syntheticOperator, process)
	if elevated && trustCode(err) != "service-is-trusted" || !elevated && err != nil {
		t.Fatalf("distinct operator (elevated=%v): %v", elevated, err)
	}
	// The bootstrap trust accepts the synthetic service token, which holds
	// no installer identity.
	if _, err := newBootstrapTrust(syntheticServiceToken(t)); err != nil {
		t.Fatal(err)
	}
	if !elevated {
		if _, err := newBootstrapTrust(process); err != nil {
			t.Fatalf("bootstrap trust for an unelevated service: %v", err)
		}
	}
}

// Native account-type resolution, without the seam: an individual user
// resolves as a user; groups, aliases, well-known, capability and unmapped
// SIDs never do, so they cannot be the named operator.
func TestTheOperatorAccountTypeIsResolvedNatively(t *testing.T) {
	sid := func(s string) *windows.SID {
		value, err := windows.StringToSid(s)
		if err != nil {
			t.Fatal(err)
		}
		return value
	}
	if use, err := sidAccountType(currentUserSID(t)); err != nil || use != windows.SidTypeUser {
		t.Fatalf("the test user: %d %v", use, err)
	}
	for _, s := range []string{
		"S-1-5-32-544", // Administrators (alias)
		"S-1-5-32-545", // Users (alias)
		"S-1-5-11",     // Authenticated Users (well-known group)
		"S-1-15-3-1",   // internetClient capability
		"S-1-15-2-1",   // all application packages
	} {
		if use, err := sidAccountType(sid(s)); err == nil && use == windows.SidTypeUser {
			t.Fatalf("%s resolved as a user", s)
		}
	}
	if _, err := sidAccountType(sid(syntheticUnknown)); err == nil {
		t.Fatal("an unmapped SID resolved")
	}
	process, err := serviceToken()
	if err != nil {
		t.Fatal(err)
	}
	defer process.Close()
	for _, s := range []string{"S-1-15-3-1", syntheticOperator, syntheticUnknown} {
		if _, err := newPolicyTrust(s, process); err == nil {
			t.Fatalf("%s accepted as the named operator", s)
		}
	}
}

// R7: the bootstrap configuration is judged by the installer trust alone; a
// configuration cannot make its own writer trusted by naming it operator.
func TestTheBootstrapConfigCannotBlessItsOperator(t *testing.T) {
	bootstrap, err := newBootstrapTrust(syntheticServiceToken(t))
	if err != nil {
		t.Fatal(err)
	}
	named := syntheticOperator // what the configuration would name
	installer := "(A;;FA;;;SY)(A;;FA;;;BA)(A;;FR;;;" + syntheticService + ")"
	for _, c := range []struct {
		name, owner, dacl, code string
	}{
		{"installer only", "BA", installer, ""},
		{"named operator may write", "BA", installer + "(A;;FW;;;" + named + ")", "policy-writable"},
		{"named operator may re-permission", "BA", installer + "(A;;WD;;;" + named + ")", "policy-writable"},
		{"named operator owns it", named, installer, "policy-owner-untrusted"},
		{"another user may write", "BA", installer + "(A;;FW;;;" + syntheticGroup + ")", "policy-writable"},
	} {
		sd, err := windows.SecurityDescriptorFromString("O:" + c.owner + "G:SYD:P" + c.dacl)
		if err != nil {
			t.Fatal(err)
		}
		if got := trustCode(bootstrap.verifyDescriptor(sd, rolePolicyFile)); got != c.code {
			t.Errorf("%s: %q, want %q", c.name, got, c.code)
		}
	}
	// The same operator-owned descriptor is acceptable for the policy once the
	// operator comes from a verified configuration.
	policy := descriptorTrust(t)
	sd, err := windows.SecurityDescriptorFromString("O:" + currentUserSID(t).String() + "G:SYD:P" + installer)
	if err != nil {
		t.Fatal(err)
	}
	if err := policy.verifyDescriptor(sd, rolePolicyFile); err != nil {
		t.Fatalf("operator-owned policy: %v", err)
	}
}

// R8: the configuration is parsed from the same held handle that was checked,
// and the checked objects cannot be replaced in between.
func TestTheSharedConfigIsParsedFromTheCheckedHandle(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "executor.json")
	want := []byte(`{"root":"C:\\checked"}`)
	if err := os.WriteFile(path, want, 0600); err != nil {
		t.Fatal(err)
	}
	var attempts []string
	configReadHook = func() {
		try := func(name string, err error) {
			if err == nil {
				attempts = append(attempts, name)
			}
		}
		try("rename the file", os.Rename(path, path+".moved"))
		try("delete the file", os.Remove(path))
		w, err := os.OpenFile(path, os.O_WRONLY, 0)
		if err == nil {
			w.Close()
		}
		try("open the file for writing", err)
		try("rename the folder", os.Rename(dir, dir+"-moved"))
	}
	defer func() { configReadHook = nil }()
	var cfg ExecutorConfig
	if err := loadSharedConfig(path, acceptTrust{}, &cfg); err != nil {
		t.Fatal(err)
	}
	configReadHook = nil
	if len(attempts) != 0 {
		t.Fatalf("succeeded while held: %v", attempts)
	}
	if cfg.Root != `C:\checked` {
		t.Fatalf("parsed %q", cfg.Root)
	}
	refuse := func(name, p, code string) {
		t.Helper()
		var cfg ExecutorConfig
		if got := trustCode(loadSharedConfig(p, acceptTrust{}, &cfg)); got != code {
			t.Fatalf("%s: %q, want %q", name, got, code)
		}
	}
	refuse("checked again", path, "")
	var none ExecutorConfig
	if err := loadSharedConfig(path, nil, &none); trustCode(err) != "config-trust-missing" {
		t.Fatalf("no trust: %v", err)
	}
	refuse("a folder", dir, "config-not-plain")
	refuse("missing", filepath.Join(dir, "absent.json"), "config-unavailable")
	big := filepath.Join(dir, "big.json")
	if err := os.WriteFile(big, []byte(strings.Repeat(" ", maxControlBytes+1)), 0600); err != nil {
		t.Fatal(err)
	}
	refuse("oversized", big, "config-too-large")
	if err := os.Link(path, path+".link"); err != nil {
		t.Fatal(err)
	}
	refuse("a second hard link", path, "config-not-plain")
	_ = os.Remove(path + ".link")
	junction := dir + "-junction"
	if out, err := exec.Command("cmd", "/d", "/c", "mklink", "/J", junction, dir).CombinedOutput(); err != nil {
		t.Fatalf("junction: %v %s", err, out)
	}
	defer os.Remove(junction)
	refuse("through a junction", filepath.Join(junction, "executor.json"), "config-not-plain")
	unknown := filepath.Join(dir, "unknown.json")
	if err := os.WriteFile(unknown, []byte(`{"root":"x","surprise":1}`), 0600); err != nil {
		t.Fatal(err)
	}
	if err := loadSharedConfig(unknown, acceptTrust{}, &none); err == nil {
		t.Fatal("parsed an unknown field")
	}
	// The bootstrap verdict is what gates the parse: a refusing trust leaves
	// the value untouched.
	var untouched ExecutorConfig
	if err := loadSharedConfig(path, refuseTrust{}, &untouched); err == nil || untouched.Root != "" {
		t.Fatalf("parsed despite refusal: %v %q", err, untouched.Root)
	}
}

type refuseTrust struct{}

func (refuseTrust) verify([]*os.File, *os.File) error       { return accessCode("refused") }
func (refuseTrust) verifyConfig([]*os.File, *os.File) error { return accessCode("refused") }

// Q8, on descriptors: owner, broad writers, deny entries, inheritance and the
// rights each role in the chain may not grant.
func TestPolicyDescriptorDecisions(t *testing.T) {
	trust := descriptorTrust(t)
	op := currentUserSID(t).String()
	base := "(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;" + op + ")(A;;FR;;;" + syntheticService + ")"
	sd := func(owner, dacl string) *windows.SECURITY_DESCRIPTOR {
		value, err := windows.SecurityDescriptorFromString("O:" + owner + "G:SYD:P" + dacl)
		if err != nil {
			t.Fatalf("sddl %s %s: %v", owner, dacl, err)
		}
		return value
	}
	all := []trustRole{rolePolicyFile, rolePolicyFolder, roleAncestor, roleVolumeRoot}
	cases := []struct {
		name  string
		owner string
		dacl  string
		roles []trustRole
		code  string
	}{
		{"operator only", op, base, all, ""},
		{"Administrators owner", "BA", base, all, ""},
		{"SYSTEM owner", "SY", base, all, ""},
		{"TrustedInstaller owner", sidTrustedInstaller, base, all, ""},
		{"service owner", syntheticService, base, all, "policy-owner-untrusted"},
		{"Users owner", "BU", base, all, "policy-owner-untrusted"},
		{"Users write", op, base + "(A;;FW;;;BU)", []trustRole{rolePolicyFile, rolePolicyFolder}, "policy-writable"},
		// Generic file write on a folder adds entries; beside a held chain
		// that replaces nothing.
		{"Users write a higher folder", op, base + "(A;;FW;;;BU)", []trustRole{roleAncestor, roleVolumeRoot}, ""},
		{"Everyone full", op, base + "(A;;FA;;;WD)", all, "policy-writable"},
		{"Authenticated Users modify", op, base + "(A;;0x1301bf;;;AU)", []trustRole{rolePolicyFile, rolePolicyFolder, roleAncestor}, "policy-writable"},
		// The default grant of a secondary NTFS volume's root: delete is moot.
		{"Authenticated Users modify the volume root", op, base + "(A;;0x1301bf;;;AU)", []trustRole{roleVolumeRoot}, ""},
		{"all application packages write", op, base + "(A;;FW;;;AC)", []trustRole{rolePolicyFile, rolePolicyFolder}, "policy-writable"},
		{"service write", op, base + "(A;;FW;;;" + syntheticService + ")", []trustRole{rolePolicyFile, rolePolicyFolder}, "policy-writable"},
		{"Users re-permission", op, base + "(A;;WD;;;BU)", all, "policy-writable"},
		{"Users take ownership", op, base + "(A;;WO;;;BU)", all, "policy-writable"},
		{"Users generic all", op, base + "(A;;GA;;;BU)", all, "policy-writable"},
		{"a deny does not excuse an allow", op, "(D;;FA;;;BU)" + base + "(A;;FA;;;BU)", all, "policy-writable"},
		{"Users delete", op, base + "(A;;SD;;;BU)", []trustRole{rolePolicyFile, rolePolicyFolder, roleAncestor}, "policy-writable"},
		{"Users delete the volume root", op, base + "(A;;SD;;;AU)", []trustRole{roleVolumeRoot}, ""},
		{"Users delete a child", op, base + "(A;;0x40;;;BU)", []trustRole{rolePolicyFolder, roleAncestor, roleVolumeRoot}, "policy-writable"},
		{"Users add entries to an ancestor", op, base + "(A;;0x6;;;BU)", []trustRole{roleAncestor, roleVolumeRoot}, ""},
		{"Users add entries to the policy folder", op, base + "(A;;0x6;;;BU)", []trustRole{rolePolicyFolder}, "policy-writable"},
		{"Users write attributes of the file", op, base + "(A;;0x100;;;BU)", []trustRole{rolePolicyFile}, "policy-writable"},
		{"Users read only", op, base + "(A;;FR;;;BU)(A;;FR;;;WD)(A;;FR;;;AC)", all, ""},
		{"an inherit-only grant applies to children", op, base + "(A;OICIIO;FA;;;BU)", all, ""},
		{"no DACL", op, "", nil, ""},
	}
	for _, c := range cases {
		for _, role := range c.roles {
			if got := trustCode(trust.verifyDescriptor(sd(c.owner, c.dacl), role)); got != c.code {
				t.Errorf("%s (%s): %q, want %q", c.name, role, got, c.code)
			}
		}
	}
	// A null DACL grants everything to everyone.
	null, err := windows.SecurityDescriptorFromString("O:" + op + "G:SYD:NO_ACCESS_CONTROL")
	if err != nil {
		t.Fatal(err)
	}
	if got := trustCode(trust.verifyDescriptor(null, rolePolicyFile)); got != "policy-writable" {
		t.Fatalf("null DACL: %q", got)
	}
	// An allow entry of a kind the check does not model is refused.
	callback, err := windows.SecurityDescriptorFromString("O:" + op + "G:SYD:P" + base + `(XA;;FR;;;BU;(Exists x))`)
	if err == nil {
		if got := trustCode(trust.verifyDescriptor(callback, rolePolicyFile)); got != "policy-ace-unsupported" {
			t.Fatalf("callback allow entry: %q", got)
		}
	} else {
		t.Logf("conditional entries unavailable in SDDL here: %v", err)
	}
	// The refusal names the role and principal for the start log.
	err = trust.verifyDescriptor(sd(op, base+"(A;;0x40;;;BU)"), roleAncestor)
	if detail := trustDetail(err).Error(); !strings.Contains(detail, "ancestor") || !strings.Contains(detail, "Users") {
		t.Fatalf("detail: %s", detail)
	}
}

// The effective-rights layer asks Windows, through the service token, which
// includes inheritance, deny entries and the restricting SID.
func TestServiceRightsComeFromAccessCheck(t *testing.T) {
	token := syntheticServiceToken(t)
	check := func(dacl string) uint32 {
		sd, err := windows.SecurityDescriptorFromString("O:SYG:SYD:P" + dacl)
		if err != nil {
			t.Fatal(err)
		}
		granted, err := effectiveRights(sd, token)
		if err != nil {
			t.Fatal(err)
		}
		return granted
	}
	if got := check("(A;;FA;;;WD)(A;;FW;;;" + syntheticService + ")"); got&policyFileRights == 0 {
		t.Fatalf("a write grant to the service was not effective: %#x", got)
	}
	if got := check("(A;;FA;;;WD)(A;;FR;;;" + syntheticService + ")"); got&policyFileRights != 0 {
		t.Fatalf("a read-only service holds write rights: %#x", got)
	}
	if got := check("(D;;FW;;;" + syntheticService + ")(A;;FA;;;WD)(A;;FA;;;" + syntheticService + ")"); got&windows.FILE_WRITE_DATA != 0 {
		t.Fatalf("a deny entry was ignored: %#x", got)
	}
}

// Q8, volumes: only fixed NTFS or ReFS volumes carry a trustworthy DACL.
func TestThePolicyVolumeMustBeLocalWithACLs(t *testing.T) {
	for _, c := range []struct {
		fs    string
		drive uint32
		ok    bool
	}{
		{"NTFS", windows.DRIVE_FIXED, true},
		{"ReFS", windows.DRIVE_FIXED, true},
		{"FAT32", windows.DRIVE_FIXED, false},
		{"exFAT", windows.DRIVE_FIXED, false},
		{"NTFS", windows.DRIVE_REMOTE, false},
		{"NTFS", windows.DRIVE_REMOVABLE, false},
		{"NTFS", windows.DRIVE_CDROM, false},
		{"NTFS", windows.DRIVE_RAMDISK, false},
		{"NTFS", windows.DRIVE_UNKNOWN, false},
	} {
		if err := volumeAllowed(c.fs, c.drive); (err == nil) != c.ok {
			t.Errorf("%s on drive type %d: %v", c.fs, c.drive, err)
		}
	}
	if driveType(`\\server\share\policy.json`) != windows.DRIVE_UNKNOWN {
		t.Fatal("a UNC path has a drive type")
	}
	if _, err := readPolicyFile(`\\127.0.0.1\share\policy.json`, acceptTrust{}); err == nil {
		t.Fatal("read a policy over the network")
	}
}

// Q8, replacement race: while the checked objects are held, the file cannot
// be written, renamed or deleted and its folders cannot be renamed, so the
// bytes read are those of the object that was checked.
func TestThePolicyReadHoldsTheCheckedObjects(t *testing.T) {
	f := newSharedFixture(t)
	want, err := os.ReadFile(f.policyPath)
	if err != nil {
		t.Fatal(err)
	}
	folder := filepath.Dir(f.policyPath)
	var attempts []string
	policyReadHook = func() {
		try := func(name string, err error) {
			if err == nil {
				attempts = append(attempts, name)
			}
		}
		try("rename the file", os.Rename(f.policyPath, f.policyPath+".moved"))
		try("delete the file", os.Remove(f.policyPath))
		w, err := os.OpenFile(f.policyPath, os.O_WRONLY, 0)
		if err == nil {
			w.Close()
		}
		try("open the file for writing", err)
		try("rename the folder", os.Rename(folder, folder+"-moved"))
		try("rename the folder's parent", os.Rename(filepath.Dir(folder), filepath.Dir(folder)+"-moved"))
	}
	defer func() { policyReadHook = nil }()
	got, err := readPolicyFile(f.policyPath, acceptTrust{})
	if err != nil {
		t.Fatal(err)
	}
	if len(attempts) != 0 {
		t.Fatalf("succeeded while held: %v", attempts)
	}
	if string(got) != string(want) {
		t.Fatal("read other bytes than the checked file")
	}
}

// No trust, no policy: the read path never runs without the checks.
func TestThePolicyIsRefusedWithoutTrust(t *testing.T) {
	f := newSharedFixture(t)
	f.s.access.trust = nil
	if _, err := f.s.currentPolicy(); err == nil || err.Error() != "policy-trust-missing" {
		t.Fatalf("no trust: %v", err)
	}
	if got := f.call("alice-laptop", "GET", "/executor/v1/jobs", nil, nil); got.status == 200 {
		t.Fatal("served without trust")
	}
}

// countingTrust records which held objects reach the real descriptor check.
type countingTrust struct {
	inner    *windowsPolicyTrust
	objects  int
	verdicts []string
}

func (c *countingTrust) verifyChain(chain []*os.File, file *os.File, folderRole trustRole) error {
	for i, f := range append(append([]*os.File{}, chain...), file) {
		role := roleAncestor
		switch {
		case i == len(chain):
			role = rolePolicyFile
		case i == len(chain)-1:
			role = folderRole
		case i == 0:
			role = roleVolumeRoot
		}
		c.objects++
		c.verdicts = append(c.verdicts, trustCode(c.inner.verifyHandle(f, role)))
	}
	return c.inner.verifyChain(chain, file, folderRole)
}
func (c *countingTrust) verify(chain []*os.File, file *os.File) error {
	return c.verifyChain(chain, file, rolePolicyFolder)
}
func (c *countingTrust) verifyConfig(chain []*os.File, file *os.File) error {
	return c.verifyChain(chain, file, roleAncestor)
}

// The real chain of the test's temporary policy and configuration, read
// without changing any ACL or owner. The verdict on real folders depends on
// the machine (a protected chain may legitimately pass), so the deterministic
// assertions use a trust that trusts nobody: every real descriptor read
// through the held handles must then refuse, at the volume root first. If
// readPolicyFile or loadSharedConfig skipped the real-chain verification,
// these reads would succeed and the test fails.
func TestTheRealPolicyChainIsChecked(t *testing.T) {
	f := newSharedFixture(t)
	nobody := &windowsPolicyTrust{nil, syntheticServiceToken(t)}
	components := 0
	for p := filepath.Clean(f.policyPath); p != filepath.Dir(p); p = filepath.Dir(p) {
		components++
	}
	counting := &countingTrust{inner: nobody}
	_, err := readPolicyFile(f.policyPath, counting)
	if code := trustCode(err); code != "policy-owner-untrusted" || !strings.Contains(trustDetail(err).Error(), "volume root") {
		t.Fatalf("the real policy chain under a trust of nobody: %v", trustDetail(err))
	}
	// Every held folder from the volume root down and the file itself were
	// read through their handles and refused on their real owner.
	if counting.objects != components+1 {
		t.Fatalf("checked %d objects for %d path components", counting.objects, components)
	}
	for i, verdict := range counting.verdicts {
		if verdict != "policy-owner-untrusted" {
			t.Fatalf("object %d: real descriptor gave %q", i, verdict)
		}
	}
	// The verdict with the test user as operator depends on this machine's
	// real ACLs; it must be a trust decision, either way.
	if _, err := readPolicyFile(f.policyPath, descriptorTrust(t)); err != nil {
		var refusal *trustError
		if !errors.As(err, &refusal) {
			t.Fatalf("the real policy chain failed outside trust: %v", err)
		}
		t.Logf("real policy chain with the test user as operator: %s", refusal.Detail())
	} else {
		t.Log("real policy chain with the test user as operator: accepted (a protected chain)")
	}
	config := filepath.Join(filepath.Dir(f.policyPath), "executor.json")
	if err := os.WriteFile(config, []byte("{}"), 0600); err != nil {
		t.Fatal(err)
	}
	counting = &countingTrust{inner: nobody}
	var cfg ExecutorConfig
	err = loadSharedConfig(config, counting, &cfg)
	if code := trustCode(err); code != "policy-owner-untrusted" || counting.objects != components+1 {
		t.Fatalf("the real configuration chain under a trust of nobody: %v (%d objects)", trustDetail(err), counting.objects)
	}
	// The production bootstrap trusts exactly the installer SIDs.
	token := syntheticServiceToken(t)
	bootstrap, err := sharedBootstrap(token)
	if err != nil {
		t.Fatal(err)
	}
	installer, _ := installerSIDs()
	got := bootstrap.(*windowsPolicyTrust).trusted
	if len(got) != len(installer) {
		t.Fatalf("bootstrap trusts %d principals", len(got))
	}
	for i := range got {
		if !got[i].Equals(installer[i]) {
			t.Fatalf("bootstrap trusts %s", got[i])
		}
	}
}
