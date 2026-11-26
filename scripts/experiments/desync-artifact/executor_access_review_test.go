package main

// Regression tests for the coordinator's M1 review (R1-R5).

import (
	"encoding/base64"
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// R3: a canonical y with no point on the curve is refused, as dalek refuses it.
func TestAnUndecompressibleKeyIsRefused(t *testing.T) {
	raw := make([]byte, 32)
	raw[0] = 2
	if _, ok := personKey(base64.RawURLEncoding.EncodeToString(raw)); ok {
		t.Fatal("accepted a key with no point on the curve")
	}
	var keys struct{ Persons []fixturePerson }
	hostAccessFixture(t, "synthetic-keys.json", &keys)
	for _, p := range keys.Persons {
		if _, ok := personKey(p.PublicKey); !ok {
			t.Fatalf("refused the valid fixture key of %s", p.Name)
		}
	}
}

// R1: a lost or unreadable durable mark on a root that already served several
// people never restarts the monotonic floor.
func TestALostHighWaterMarkCannotRebootstrap(t *testing.T) {
	for _, restart := range []bool{false, true} {
		f := newSharedFixture(t)
		old, err := os.ReadFile(f.policyPath)
		if err != nil {
			t.Fatal(err)
		}
		f.revision = 2
		f.writePolicy()
		if _, err := f.s.currentPolicy(); err != nil {
			t.Fatal(err)
		}
		mark := filepath.Join(f.s.cfg.Root, "access-high-water.json")
		if err := os.Remove(mark); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(f.policyPath, old, 0600); err != nil {
			t.Fatal(err)
		}
		service := f.s
		if restart {
			service = f.service(f.s.cfg)
		}
		if _, err := service.currentPolicy(); err == nil {
			t.Fatalf("restart=%v: an older policy was accepted after the mark was lost", restart)
		}
		// Even the current revision is refused until the operator restores the
		// mark: the floor is unknown.
		f.writePolicy()
		if _, err := f.service(f.s.cfg).currentPolicy(); err == nil || err.Error() != "high-water-lost" {
			t.Fatalf("restart=%v: lost mark: %v", restart, err)
		}
	}
	// An unreadable mark is not a missing one.
	f := newSharedFixture(t)
	if err := os.WriteFile(filepath.Join(f.s.cfg.Root, "access-high-water.json"), []byte("{"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := f.service(f.s.cfg).currentPolicy(); err == nil {
		t.Fatal("a corrupt mark reset the floor")
	}
}

// R1 (re-review): with unchanged policy bytes, in the same process, the cached
// verification never skips the durable mark, mode record or floor checks.
func TestTheCachedPolicyStillChecksTheMarkAndMode(t *testing.T) {
	markPath := func(f *sharedFixture) string { return filepath.Join(f.s.cfg.Root, "access-high-water.json") }
	modePath := func(f *sharedFixture) string { return filepath.Join(f.s.cfg.Root, "access-mode.json") }
	write := func(t *testing.T, path string, value any) {
		data, err := json.Marshal(value)
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, data, 0600); err != nil {
			t.Fatal(err)
		}
	}
	cases := []struct {
		name   string
		code   string
		damage func(t *testing.T, f *sharedFixture)
	}{
		{"mark missing", "high-water-lost", func(t *testing.T, f *sharedFixture) {
			if err := os.Remove(markPath(f)); err != nil {
				t.Fatal(err)
			}
		}},
		{"mark corrupt", "high-water-unreadable", func(t *testing.T, f *sharedFixture) {
			if err := os.WriteFile(markPath(f), []byte("{"), 0600); err != nil {
				t.Fatal(err)
			}
		}},
		{"mark emptied", "high-water-unreadable", func(t *testing.T, f *sharedFixture) {
			write(t, markPath(f), accessHighWater{deliveryProtocol, f.s.identity, 0, ""})
		}},
		{"mark replaced", "high-water-replaced", func(t *testing.T, f *sharedFixture) {
			write(t, markPath(f), accessHighWater{deliveryProtocol, f.s.identity, f.s.access.floorRevision, strings.Repeat("0", 64)})
		}},
		{"mark ahead", "policy-rollback", func(t *testing.T, f *sharedFixture) {
			write(t, markPath(f), accessHighWater{deliveryProtocol, f.s.identity, f.s.access.floorRevision + 1, strings.Repeat("0", 64)})
		}},
		{"mode missing", "access-mode-lost", func(t *testing.T, f *sharedFixture) {
			if err := os.Remove(modePath(f)); err != nil {
				t.Fatal(err)
			}
		}},
		{"mode corrupt", "access-mode-unreadable", func(t *testing.T, f *sharedFixture) {
			if err := os.WriteFile(modePath(f), []byte("{"), 0600); err != nil {
				t.Fatal(err)
			}
		}},
		{"mode changed", "access-mode-changed", func(t *testing.T, f *sharedFixture) {
			write(t, modePath(f), accessModeRecord{deliveryProtocol, "single", f.s.identity})
		}},
	}
	for _, c := range cases {
		f := newSharedFixture(t)
		before, err := os.ReadFile(f.policyPath)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := f.s.currentPolicy(); err != nil {
			t.Fatalf("%s: warm cache: %v", c.name, err)
		}
		c.damage(t, f)
		_, err = f.s.currentPolicy()
		if err == nil || err.Error() != c.code {
			t.Fatalf("%s: same service, unchanged policy: %v, want %s", c.name, err, c.code)
		}
		// The jobs routes see the same refusal.
		if got := f.call("alice-laptop", "GET", "/executor/v1/jobs", nil, nil); got.status == 200 {
			t.Fatalf("%s: the list was served: %s", c.name, got.body)
		}
		after, _ := os.ReadFile(f.policyPath)
		if string(after) != string(before) {
			t.Fatalf("%s: the policy bytes changed", c.name)
		}
	}
	// A mark rolled back below the floor of this process, with the policy and
	// cache unchanged.
	f := newSharedFixture(t)
	f.revision = 2
	f.writePolicy()
	if _, err := f.s.currentPolicy(); err != nil {
		t.Fatal(err)
	}
	f.s.access.floorRevision = 3 // as if this process had accepted revision 3 earlier
	if _, err := f.s.currentPolicy(); err == nil || err.Error() != "high-water-rollback" {
		t.Fatalf("mark below floor: %v", err)
	}
}

// R2: the list applies the current read scope of each row's project.
func TestTheListRechecksCurrentProjectMembership(t *testing.T) {
	f := newSharedFixture(t)
	job := newJob("membership")
	expect(t, f.submit("alice-laptop", job, "proj-shared"), 200, "submit")
	var policy map[string]any
	if err := json.Unmarshal(f.policyBytes(), &policy); err != nil {
		t.Fatal(err)
	}
	for _, entry := range policy["projects"].([]any) {
		project := entry.(map[string]any)
		if project["project"] == "proj-shared" {
			project["members"] = []any{map[string]any{"person": f.person("bob").Person, "role": "contributor"}}
		}
	}
	policy["revision"] = 2
	data, _ := json.Marshal(policy)
	if err := os.WriteFile(f.policyPath, data, 0600); err != nil {
		t.Fatal(err)
	}
	got := f.call("alice-laptop", "GET", "/executor/v1/jobs", nil, nil)
	if got.status != 200 || strings.Contains(got.body, job) {
		t.Fatalf("list after removal from the project: %d %s", got.status, got.body)
	}
	expect(t, f.call("alice-laptop", "GET", "/executor/v1/jobs/"+job, nil, nil), 403, "per-job read after removal")
}

// R4: revocation after deployment began, or at the launch boundary, never
// reaches native creation.
func TestRevocationDuringDeploymentOrAtLaunchPreventsTheLaunch(t *testing.T) {
	for _, revokeAt := range []string{"slots", "bundles", "launch"} {
		f := newSharedFixture(t)
		job := newJob("deploy-" + revokeAt)
		expect(t, f.submit("alice-laptop", job, "proj-shared"), 200, "submit")
		revoke := func() {
			f.revision++
			f.revokedDevices = []string{f.devices["alice-laptop"].fingerprint}
			f.writePolicy()
		}
		launched := false
		var admission error
		f.s.deliverFn = func(value ExecutorSubmission, offer TreeOffer, kind string) (string, string, error) {
			if kind == revokeAt {
				revoke()
			}
			return filepath.Join(f.s.cfg.Root, "fake-"+kind), strings.Repeat("e", 32), nil
		}
		f.s.launchFn = func(command, path string) error {
			if revokeAt == "launch" {
				// What the scenario sees after its verification and quiet
				// wait: the boundary check immediately before creation.
				revoke()
				if admission = admitExecution(job); admission != nil {
					return admission
				}
			}
			launched = true
			return nil
		}
		value, err := f.s.readSubmission(job)
		if err != nil {
			t.Fatal(err)
		}
		f.s.runOne(value)
		after, _ := f.s.readSubmission(job)
		if launched {
			t.Fatalf("%s: launched after revocation (%s)", revokeAt, after.Phase)
		}
		switch revokeAt {
		case "launch":
			if admission == nil || admission.Error() != "device-revoked" {
				t.Fatalf("launch boundary admission: %v", admission)
			}
		default:
			if after.Phase != "access_refused" || !strings.Contains(after.Reason, "device-revoked") {
				t.Fatalf("%s: %s %s", revokeAt, after.Phase, after.Reason)
			}
			if _, err := os.Stat(filepath.Join(f.s.cfg.Root, "requests", job, "run.json")); !os.IsNotExist(err) {
				t.Fatalf("%s: a run configuration was written", revokeAt)
			}
		}
		// The registration ends with the run; afterwards a shared executor
		// refuses an unregistered launch.
		if err := admitExecution(job); err == nil || err.Error() != "admission-missing" {
			t.Fatalf("%s: admission after the run: %v", revokeAt, err)
		}
	}
}

// R4: a refusal from the creation callback, which runs before CreateProcess,
// creates no process at all.
func TestARefusedCreationCallbackCreatesNoProcess(t *testing.T) {
	root := t.TempDir()
	log, err := os.Create(filepath.Join(root, "log"))
	if err != nil {
		t.Fatal(err)
	}
	defer log.Close()
	native, err := newNativeScenario(128 * MiB)
	if err != nil {
		t.Fatal(err)
	}
	defer native.Close()
	system, err := systemDirectory()
	if err != nil {
		t.Fatal(err)
	}
	marker := filepath.Join(root, "marker.txt")
	calls := 0
	err = native.Start(filepath.Join(system, "cmd.exe"), []string{"/d", "/c", "echo created>" + marker}, map[string]string{"SystemRoot": filepath.Dir(system)}, root, log,
		func(info processReceipt) error {
			calls++
			if !info.Accepted {
				return accessCode("device-revoked")
			}
			return nil
		})
	if err == nil || calls != 1 || native.process != 0 {
		t.Fatalf("start after refusal: err=%v calls=%d process=%v", err, calls, native.process)
	}
	if active, err := jobActive(native.job); err != nil || active != 0 {
		t.Fatalf("job has %d processes (%v)", active, err)
	}
	if _, err := os.Stat(marker); !os.IsNotExist(err) {
		t.Fatal("the refused process ran")
	}
}

// R5: the policy is read through one no-follow handle; links, junctioned
// ancestors, hard links and directories are refused.
func TestThePolicyReadFollowsNoLinkOrJunction(t *testing.T) {
	f := newSharedFixture(t)
	if _, err := readPolicyFile(f.policyPath, acceptTrust{}); err != nil {
		t.Fatalf("plain policy: %v", err)
	}
	operator := filepath.Dir(f.policyPath)
	// A junction ancestor resolves elsewhere: refused.
	junction := filepath.Join(filepath.Dir(operator), "junction")
	if out, err := exec.Command("cmd", "/d", "/c", "mklink", "/J", junction, operator).CombinedOutput(); err != nil {
		t.Fatalf("junction: %v %s", err, out)
	}
	if _, err := readPolicyFile(filepath.Join(junction, filepath.Base(f.policyPath)), acceptTrust{}); err == nil {
		t.Fatal("read the policy through a junction")
	}
	// A symbolic link at the policy name is the link, not its target.
	link := filepath.Join(operator, "linked-policy.json")
	if err := os.Symlink(f.policyPath, link); err == nil {
		if _, err := readPolicyFile(link, acceptTrust{}); err == nil {
			t.Fatal("followed a symbolic link")
		}
	} else {
		t.Logf("symbolic links need a privilege here; the junction case covers reparse refusal: %v", err)
	}
	// A second hard link could be written from elsewhere.
	hard := filepath.Join(operator, "hard-link.json")
	if err := os.Link(f.policyPath, hard); err != nil {
		t.Fatal(err)
	}
	if _, err := readPolicyFile(f.policyPath, acceptTrust{}); err == nil {
		t.Fatal("read a policy that has a second hard link")
	}
	_ = os.Remove(hard)
	if _, err := readPolicyFile(operator, acceptTrust{}); err == nil {
		t.Fatal("read a directory")
	}
	if _, err := readPolicyFile(f.policyPath, acceptTrust{}); err != nil {
		t.Fatalf("policy after the link was removed: %v", err)
	}
}
