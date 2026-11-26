package main

// Job, handle and environment hardening of the scenario launcher, and its
// legacy behavior: creation receipts, settlement, deadlines, logs and unknown
// outcomes. Synthetic children only: cmd.exe one-liners and this test binary
// re-run as a probe. Kernel objects are test-local; no real credential,
// account or ACL is read or changed.

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"testing"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

func nativeFixture(t *testing.T) (*nativeScenario, *os.File, string, string) {
	t.Helper()
	root := t.TempDir()
	log, err := os.Create(filepath.Join(root, "log"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { log.Close() })
	native, err := newNativeScenario(256 * MiB)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(native.Close)
	system, err := systemDirectory()
	if err != nil {
		t.Fatal(err)
	}
	return native, log, root, system
}

func baseEnvironment(system string) map[string]string {
	return map[string]string{"SystemRoot": filepath.Dir(system), "COMSPEC": filepath.Join(system, "cmd.exe"), "PATH": system}
}

// 1. Named scenario jobs carry an explicit protected DACL: the executor's
// account and SYSTEM, nothing else, and the service keeps every right it uses.
func TestScenarioJobsHaveAProtectedServiceOnlyDACL(t *testing.T) {
	native, _, _, _ := nativeFixture(t)
	sd, err := windows.GetSecurityInfo(native.job, windows.SE_KERNEL_OBJECT,
		windows.OWNER_SECURITY_INFORMATION|windows.GROUP_SECURITY_INFORMATION|windows.DACL_SECURITY_INFORMATION)
	if err != nil {
		t.Fatal(err)
	}
	control, _, err := sd.Control()
	if err != nil || control&windows.SE_DACL_PROTECTED == 0 {
		t.Fatalf("job DACL is not protected: %#x %v", control, err)
	}
	dacl, _, err := sd.DACL()
	if err != nil || dacl == nil {
		t.Fatalf("job has no DACL: %v", err)
	}
	me := currentUserSID(t)
	system, _ := windows.StringToSid(sidSystem)
	if dacl.AceCount != 2 {
		t.Fatalf("job DACL has %d entries: %s", dacl.AceCount, sd)
	}
	for i := 0; i < int(dacl.AceCount); i++ {
		var ace *windows.ACCESS_ALLOWED_ACE
		if err := windows.GetAce(dacl, uint32(i), &ace); err != nil {
			t.Fatal(err)
		}
		sid := (*windows.SID)(unsafe.Pointer(&ace.SidStart))
		if ace.Header.AceType != windows.ACCESS_ALLOWED_ACE_TYPE || !(sid.Equals(me) || sid.Equals(system)) {
			t.Fatalf("unexpected job DACL entry %d: %s", i, sd)
		}
		if ace.Mask&0x1f001f != 0x1f001f { // JOB_OBJECT_ALL_ACCESS
			t.Fatalf("entry %d lacks job rights: %#x", i, ace.Mask)
		}
	}
	// A different account (the synthetic restricted service token, which holds
	// only what both the test user's groups and its restricting SID are
	// granted) gets no right on the job.
	granted, err := effectiveRights(sd, syntheticServiceToken(t))
	if err != nil {
		t.Fatal(err)
	}
	if granted != 0 {
		t.Fatalf("another account holds job rights %#x", granted)
	}
	// The service keeps query, termination and recovery: settlement by name
	// still opens the job.
	if active, err := jobActive(native.job); err != nil || active != 0 {
		t.Fatalf("query: %d %v", active, err)
	}
	if settled, err := namedJobSettled(processReceipt{Name: native.info.Name, PID: uint32(os.Getpid()), Birth: 1, Accepted: true}); err != nil || !settled {
		t.Fatalf("named recovery query: %v %v", settled, err)
	}
	// The executor's own memory job uses the same DACL.
	security, err := jobSecurity()
	if err != nil {
		t.Fatal(err)
	}
	if got := security.SecurityDescriptor.String(); !strings.Contains(got, "D:P(A;;") || strings.Count(got, "(A;") != 2 || security.InheritHandle != 0 {
		t.Fatalf("job security: %s", got)
	}
}

// 2. The thread stays suspended until the exact accepted receipt is durable;
// a refusal at either receipt never runs the child, and nothing claims a
// settlement it has not verified.
func TestCreationReceiptsPrecedeTheFirstInstruction(t *testing.T) {
	native, log, root, system := nativeFixture(t)
	marker := filepath.Join(root, "ran.txt")
	var order []string
	err := native.Start(filepath.Join(system, "cmd.exe"), []string{"/d", "/c", "echo ran>" + marker}, baseEnvironment(system), root, log,
		func(info processReceipt) error {
			if !info.Accepted {
				order = append(order, "issued")
				if !info.Issued || info.PID != 0 {
					return errors.New("issued receipt carries a process")
				}
				return nil
			}
			order = append(order, "accepted")
			// The child exists, inside the job, and has not run.
			time.Sleep(300 * time.Millisecond)
			if _, err := os.Stat(marker); !os.IsNotExist(err) {
				return errors.New("the child ran before its accepted receipt")
			}
			if active, err := jobActive(native.job); err != nil || active != 1 {
				return fmt.Errorf("job membership at acceptance: %d %v", active, err)
			}
			if info.PID == 0 || info.Birth == 0 || !info.Issued {
				return errors.New("accepted receipt lacks the exact identity")
			}
			return nil
		})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Join(order, ",") != "issued,accepted" {
		t.Fatalf("receipt order: %v", order)
	}
	if code, timedOut, err := native.Wait(10, nil); err != nil || timedOut || code != 0 {
		t.Fatalf("wait: %d %v %v", code, timedOut, err)
	}
	if _, err := os.Stat(marker); err != nil {
		t.Fatal("the accepted child did not run")
	}

	// A refusal of the accepted receipt leaves the child suspended and never
	// resumed; its root is alive, so its settlement is not claimed.
	refused, log2, root2, _ := nativeFixture(t)
	marker2 := filepath.Join(root2, "ran.txt")
	var accepted processReceipt
	err = refused.Start(filepath.Join(system, "cmd.exe"), []string{"/d", "/c", "echo ran>" + marker2}, baseEnvironment(system), root2, log2,
		func(info processReceipt) error {
			if info.Accepted {
				accepted = info
				return errors.New("synthetic durable-write failure")
			}
			return nil
		})
	if err == nil {
		t.Fatal("a refused accepted receipt started the child")
	}
	if settled, err := namedJobSettled(accepted); settled || err != nil {
		t.Fatalf("an unresumed live child was reported settled: %v %v", settled, err)
	}
	refused.Close() // the job's kill-on-close ends the suspended child
	time.Sleep(200 * time.Millisecond)
	if _, err := os.Stat(marker2); !os.IsNotExist(err) {
		t.Fatal("the refused child ran")
	}
	if settled, err := namedJobSettled(accepted); err != nil || !settled {
		t.Fatalf("after closing its job: %v %v", settled, err)
	}
	// An issued receipt without an accepted identity is an unknown outcome:
	// never settled, never replayable.
	if settled, err := namedJobSettled(processReceipt{Name: accepted.Name, Issued: true}); settled || err == nil {
		t.Fatalf("issued-only receipt: %v %v", settled, err)
	}
}

// TestTreeProbeChild runs only as the synthetic process tree of the deadline
// test: level 1 starts level 2 (a grandchild of the launcher) and both sleep.
func TestTreeProbeChild(t *testing.T) {
	level := os.Getenv("ZERUS_TREE_PROBE")
	if level == "" {
		t.Skip("probe child only")
	}
	if level == "1" {
		self, _ := os.Executable()
		child := exec.Command(self, "-test.run=^TestTreeProbeChild$")
		child.Env = []string{"SystemRoot=" + os.Getenv("SystemRoot"), "ZERUS_TREE_PROBE=2"}
		if err := child.Start(); err != nil {
			fmt.Println("tree-probe-start-failed", err)
			os.Exit(3)
		}
		fmt.Println("before-deadline", child.Process.Pid)
	}
	time.Sleep(120 * time.Second)
	os.Exit(0)
}

// 5. Whole-tree settlement at the deadline: a grandchild that would outlive
// the root is ended with it, and the log of the tree is captured.
func TestTheDeadlineEndsTheWholeTree(t *testing.T) {
	native, log, root, system := nativeFixture(t)
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	env := map[string]string{"SystemRoot": filepath.Dir(system), "ZERUS_TREE_PROBE": "1"}
	if err := native.Start(self, []string{"-test.run=^TestTreeProbeChild$"}, env, root, log, func(processReceipt) error { return nil }); err != nil {
		t.Fatal(err)
	}
	for wait := time.Now().Add(20 * time.Second); ; time.Sleep(50 * time.Millisecond) {
		if active, _ := jobActive(native.job); active >= 2 {
			break
		}
		if time.Now().After(wait) {
			data, _ := os.ReadFile(log.Name())
			t.Fatalf("the synthetic tree did not start: %s", data)
		}
	}
	started := time.Now()
	code, timedOut, err := native.Wait(1, nil)
	if err != nil || !timedOut || code != 0xe0000001 {
		t.Fatalf("deadline: code %#x timedOut %v err %v", code, timedOut, err)
	}
	if time.Since(started) > 15*time.Second {
		t.Fatal("deadline settlement took too long")
	}
	if active, err := jobActive(native.job); err != nil || active != 0 {
		t.Fatalf("tree survived the deadline: %d %v", active, err)
	}
	if settled, err := namedJobSettled(native.info); err != nil || !settled {
		t.Fatalf("named settlement after the deadline: %v %v", settled, err)
	}
	data, _ := os.ReadFile(log.Name())
	if !strings.Contains(string(data), "before-deadline") {
		t.Fatalf("log: %q", data)
	}
}

// 5. Normal exit with its code and complete log; a stale receipt whose PID
// now belongs to another process (other birth) reads as exited.
func TestNormalExitCodeLogAndStaleReceipts(t *testing.T) {
	native, log, root, system := nativeFixture(t)
	if err := native.Start(filepath.Join(system, "cmd.exe"), []string{"/d", "/c", "echo line-one & echo line-two 1>&2 & exit 7"}, baseEnvironment(system), root, log,
		func(processReceipt) error { return nil }); err != nil {
		t.Fatal(err)
	}
	code, timedOut, err := native.Wait(10, nil)
	if err != nil || timedOut || code != 7 {
		t.Fatalf("exit: %d %v %v", code, timedOut, err)
	}
	data, _ := os.ReadFile(log.Name())
	if !strings.Contains(string(data), "line-one") || !strings.Contains(string(data), "line-two") {
		t.Fatalf("stdout and stderr were not both captured: %q", data)
	}
	self, err := currentSupervisor()
	if err != nil {
		t.Fatal(err)
	}
	if exited, err := rootExited(self); err != nil || exited {
		t.Fatalf("a live supervisor read as exited: %v %v", exited, err)
	}
	if exited, err := rootExited(processReceipt{PID: self.PID, Birth: self.Birth + 1, Accepted: true}); err != nil || !exited {
		t.Fatalf("a reused PID read as the original process: %v %v", exited, err)
	}
	if _, err := rootExited(processReceipt{PID: self.PID, Birth: self.Birth}); err == nil {
		t.Fatal("an unaccepted receipt was read as a known process")
	}
}

// 4. Environment construction: exact, bounded, case-insensitively unique and
// sorted as Windows expects.
func TestTheEnvironmentBlockIsExactAndBounded(t *testing.T) {
	block, err := environmentBlock(map[string]string{"b": "2", "A": "1", "Path": "x"})
	if err != nil {
		t.Fatal(err)
	}
	got := strings.Split(string(utf16Decode(block)), "\x00")
	if got[0] != "A=1" || got[1] != "b=2" || got[2] != "Path=x" || got[3] != "" || got[4] != "" {
		t.Fatalf("block order: %q", got)
	}
	for name, values := range map[string]map[string]string{
		"case-insensitive duplicate": {"Path": "a", "PATH": "b"},
		"empty name":                 {"": "x"},
		"equals in name":             {"A=B": "x"},
		"NUL in name":                {"A\x00": "x"},
		"NUL in value":               {"A": "x\x00y"},
		"long name":                  {strings.Repeat("N", maxEnvironmentName+1): "x"},
		"long value":                 {"A": strings.Repeat("v", maxEnvironmentValue+1)},
	} {
		if _, err := environmentBlock(values); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
	many := map[string]string{}
	for i := 0; i <= maxEnvironmentVariables; i++ {
		many["V"+strconv.Itoa(i)] = "x"
	}
	if _, err := environmentBlock(many); err == nil {
		t.Error("too many variables accepted")
	}
	large := map[string]string{}
	for i := 0; i < 3; i++ {
		large["V"+strconv.Itoa(i)] = strings.Repeat("v", maxEnvironmentValue)
	}
	if _, err := environmentBlock(large); err == nil {
		t.Error("an oversized block accepted")
	}
	if empty, err := environmentBlock(nil); err != nil || len(empty) != 2 {
		t.Fatalf("empty environment: %v %v", empty, err)
	}
}

func utf16Decode(block []uint16) []rune {
	out := make([]rune, 0, len(block))
	for _, u := range block {
		out = append(out, rune(u))
	}
	return out
}

// The scenario environment is exactly its allowlist, with job-local profile
// and temporary folders and the documented contract.
func TestTheScenarioEnvironmentIsItsAllowlist(t *testing.T) {
	work := t.TempDir()
	cfg := ScenarioConfig{ArtifactSlot: filepath.Join(work, "artifact"), ScenarioSlot: filepath.Join(work, "bundle"), Device: "console"}
	env, err := scenarioEnvironment(work, cfg)
	if err != nil {
		t.Fatal(err)
	}
	var keys []string
	for key := range env {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	want := append([]string{}, scenarioEnvironmentKeys...)
	sort.Strings(want)
	if strings.Join(keys, ",") != strings.Join(want, ",") {
		t.Fatalf("keys %v, want %v", keys, want)
	}
	runtime := filepath.Join(work, "runtime")
	for _, key := range []string{"TEMP", "TMP", "HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA"} {
		if env[key] != runtime {
			t.Fatalf("%s is %q, not job-local", key, env[key])
		}
	}
	if env["ZERUS_RESULT_DIR"] != filepath.Join(work, "outputs") || env["ZERUS_DEVICE_ID"] != "console" ||
		env["ZERUS_ARTIFACT_ROOT"] != filepath.Join(cfg.ArtifactSlot, "build") || env["ZERUS_SCENARIO_ROOT"] != filepath.Join(cfg.ScenarioSlot, "build") {
		t.Fatalf("scenario contract: %v", env)
	}
	if _, err := environmentBlock(env); err != nil {
		t.Fatal(err)
	}
}

// probeReport is what the synthetic probe child prints.
type probeReport struct {
	Stdin, Stdout string
	Leaks         []string
	Environment   []string
}

// TestHandleProbeChild is not a test on its own: it runs only as the
// synthetic child that the handle and environment tests launch.
func TestHandleProbeChild(t *testing.T) {
	if os.Getenv("ZERUS_HANDLE_PROBE") != "1" {
		t.Skip("probe child only")
	}
	sentinels := strings.ToLower(os.Getenv("ZERUS_PROBE_SENTINELS"))
	parent, _ := strconv.Atoi(os.Getenv("ZERUS_PROBE_PARENT"))
	kind := func(h windows.Handle) string {
		switch t, _ := windows.GetFileType(h); t {
		case windows.FILE_TYPE_CHAR:
			return "char"
		case windows.FILE_TYPE_PIPE:
			return "pipe"
		case windows.FILE_TYPE_DISK:
			return "disk"
		}
		return "other"
	}
	stdin, _ := windows.GetStdHandle(windows.STD_INPUT_HANDLE)
	stdout, _ := windows.GetStdHandle(windows.STD_OUTPUT_HANDLE)
	report := probeReport{Stdin: kind(stdin), Stdout: kind(stdout), Environment: os.Environ()}
	for value := uintptr(4); value < 0x10000; value += 4 {
		h := windows.Handle(value)
		var flags uint32
		if getHandleInformation(h, &flags) != nil {
			continue
		}
		if kind(h) == "disk" {
			buffer := make([]uint16, 1024)
			if n, err := windows.GetFinalPathNameByHandle(h, &buffer[0], uint32(len(buffer)), 0); err == nil && int(n) < len(buffer) {
				if path := strings.ToLower(windows.UTF16ToString(buffer[:n])); sentinels != "" && strings.Contains(path, sentinels) {
					report.Leaks = append(report.Leaks, "file "+path)
				}
			}
		}
		if pid, err := windows.GetProcessId(h); err == nil && int(pid) == parent {
			report.Leaks = append(report.Leaks, "parent process handle")
		}
		var accounting jobAccounting
		if windows.QueryInformationJobObject(h, windows.JobObjectBasicAccountingInformation, uintptr(unsafe.Pointer(&accounting)), uint32(unsafe.Sizeof(accounting)), nil) == nil {
			report.Leaks = append(report.Leaks, fmt.Sprintf("job handle %#x", value))
		}
	}
	data, _ := json.Marshal(report)
	fmt.Printf("\nPROBE %s\n", data)
	os.Exit(0)
}

// runProbe starts the probe child through the scenario launcher with the
// given environment and returns its report.
func runProbe(t *testing.T, environment map[string]string) probeReport {
	t.Helper()
	native, log, root, _ := nativeFixture(t)
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	if err := native.Start(self, []string{"-test.run=^TestHandleProbeChild$"}, environment, root, log, func(processReceipt) error { return nil }); err != nil {
		t.Fatal(err)
	}
	if code, timedOut, err := native.Wait(60, nil); err != nil || timedOut || code != 0 {
		data, _ := os.ReadFile(log.Name())
		t.Fatalf("probe: %d %v %v %s", code, timedOut, err, data)
	}
	data, _ := os.ReadFile(log.Name())
	at := strings.LastIndex(string(data), "PROBE ")
	if at < 0 {
		t.Fatalf("no probe report: %s", data)
	}
	var report probeReport
	line := strings.SplitN(string(data[at+len("PROBE "):]), "\n", 2)[0]
	if err := json.Unmarshal([]byte(strings.TrimSpace(line)), &report); err != nil {
		t.Fatalf("probe report: %v %s", err, line)
	}
	return report
}

// 3. Only the pipe writer and NUL reach the child. Inheritable sentinels
// standing for the policy, the identity key, a control record, a Job object
// and a process handle stay in the executor.
func TestOnlyTheLogPipeAndNulAreInherited(t *testing.T) {
	sentinels := t.TempDir()
	inherit := &windows.SecurityAttributes{Length: uint32(unsafe.Sizeof(windows.SecurityAttributes{})), InheritHandle: 1}
	for _, name := range []string{"policy.json", "identity.key", "executor-queue.json"} {
		path := filepath.Join(sentinels, name)
		if err := os.WriteFile(path, []byte("synthetic sentinel, not a credential"), 0600); err != nil {
			t.Fatal(err)
		}
		p, _ := windows.UTF16PtrFromString(path)
		h, err := windows.CreateFile(p, windows.GENERIC_READ, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE, inherit, windows.OPEN_EXISTING, 0, 0)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { windows.CloseHandle(h) })
	}
	job, err := windows.CreateJobObject(inherit, nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { windows.CloseHandle(job) })
	var process windows.Handle
	if err := windows.DuplicateHandle(windows.CurrentProcess(), windows.CurrentProcess(), windows.CurrentProcess(), &process, 0, true, windows.DUPLICATE_SAME_ACCESS); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { windows.CloseHandle(process) })
	system, _ := systemDirectory()
	env := baseEnvironment(system)
	env["ZERUS_HANDLE_PROBE"] = "1"
	env["ZERUS_PROBE_SENTINELS"] = filepath.Base(sentinels)
	env["ZERUS_PROBE_PARENT"] = strconv.Itoa(os.Getpid())
	report := runProbe(t, env)
	if len(report.Leaks) != 0 {
		t.Fatalf("handles reached the child: %v", report.Leaks)
	}
	if report.Stdin != "char" || report.Stdout != "pipe" {
		t.Fatalf("standard handles: stdin %s stdout %s", report.Stdin, report.Stdout)
	}
	// The probe itself can see a leak: the same report finds an inheritable
	// sentinel that a child opens on its own.
	// (Positive control: run the scan against the parent's own table.)
	if leaks := probeHandleScan(filepath.Base(sentinels), os.Getpid(), true); len(leaks) < 5 {
		t.Fatalf("the probe scan misses handles it should see in the parent: %v", leaks)
	}
}

// 4. The real scenario environment reaches the child exactly; fake provider,
// HGS, SSH and proxy secrets set in the executor's environment do not.
func TestTheChildSeesOnlyTheScenarioEnvironment(t *testing.T) {
	secrets := map[string]string{
		"ANTHROPIC_API_KEY": "fake-provider-sentinel-1", "OPENAI_API_KEY": "fake-provider-sentinel-2",
		"CODEX_HOME": "fake-provider-sentinel-3", "CLAUDE_CONFIG_DIR": "fake-provider-sentinel-4",
		"HGS_ACCOUNT": "fake-hgs-sentinel", "HGS_STATE_DIR": "fake-hgs-sentinel-2",
		"SSH_AUTH_SOCK": "fake-ssh-sentinel", "HTTPS_PROXY": "http://user:fake-proxy-sentinel@proxy.invalid",
		"GH_TOKEN": "fake-token-sentinel",
	}
	for key, value := range secrets {
		t.Setenv(key, value)
	}
	work := t.TempDir()
	if err := os.MkdirAll(filepath.Join(work, "runtime"), 0700); err != nil {
		t.Fatal(err)
	}
	env, err := scenarioEnvironment(work, ScenarioConfig{ArtifactSlot: filepath.Join(work, "a"), ScenarioSlot: filepath.Join(work, "b"), Device: "console"})
	if err != nil {
		t.Fatal(err)
	}
	env["ZERUS_HANDLE_PROBE"] = "1"
	report := runProbe(t, env)
	seen := map[string]string{}
	for _, entry := range report.Environment {
		key, value, _ := strings.Cut(entry, "=")
		if key == "" {
			continue // per-drive "=C:" entries Windows adds for the current directory
		}
		seen[strings.ToUpper(key)] = value
	}
	for key, value := range secrets {
		if _, ok := seen[strings.ToUpper(key)]; ok {
			t.Fatalf("%s reached the child", key)
		}
		for _, v := range seen {
			if strings.Contains(v, value) {
				t.Fatalf("a secret value reached the child under another name")
			}
		}
	}
	for key, value := range env {
		if seen[strings.ToUpper(key)] != value {
			t.Fatalf("%s is %q in the child, want %q", key, seen[strings.ToUpper(key)], value)
		}
	}
	for key := range seen {
		if _, ok := env[key]; ok {
			continue
		}
		found := false
		for want := range env {
			if strings.EqualFold(want, key) {
				found = true
			}
		}
		if !found {
			t.Fatalf("the child has an extra variable %s", key)
		}
	}
}
