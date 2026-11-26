package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func writeJobState(t *testing.T, root, job string, state ScenarioState) {
	t.Helper()
	if err := os.MkdirAll(filepath.Join(root, "jobs", job), 0700); err != nil {
		t.Fatal(err)
	}
	state.Job = job
	if err := durableTreeJSON(filepath.Join(root, "jobs", job, "job-state.json"), state); err != nil {
		t.Fatal(err)
	}
}

func TestLeasesShareAGenerationAndHoldAnAdvance(t *testing.T) {
	root := t.TempDir()
	gen1, gen2 := strings.Repeat("1", 64), strings.Repeat("2", 64)
	relative := "project-slots/proj-shared/game"
	if err := acquireLease(root, relative, slotLease{Generation: gen1, Job: "job-a"}); err != nil {
		t.Fatal(err)
	}
	// Another reader of the same generation shares it.
	if err := acquireLease(root, relative, slotLease{Generation: gen1, Job: "job-b"}); err != nil {
		t.Fatalf("same generation: %v", err)
	}
	// Advancing to another generation is held, also for tree-receive.
	if err := acquireLease(root, relative, slotLease{Generation: gen2, Job: "job-c"}); err == nil || err.Error() != "generation-held" {
		t.Fatalf("advance under readers: %v", err)
	}
	base := filepath.Join(root, filepath.FromSlash(relative))
	if err := leasesAllow(base, gen2, ""); err == nil {
		t.Fatal("tree-receive may advance under a reader")
	}
	if err := leasesAllow(base, gen1, ""); err != nil {
		t.Fatalf("tree-receive of the same generation: %v", err)
	}
	for _, job := range []string{"job-a", "job-b"} {
		if err := releaseLease(root, relative, job); err != nil {
			t.Fatal(err)
		}
	}
	if err := acquireLease(root, relative, slotLease{Generation: gen2, Job: "job-c"}); err != nil {
		t.Fatalf("advance after release: %v", err)
	}
	// An unreadable lease pins the slot.
	if err := os.WriteFile(filepath.Join(base, "leases", "job-x.json"), []byte("{"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := leasesAllow(base, gen2, "job-c"); err == nil {
		t.Fatal("an unreadable lease did not pin the slot")
	}
}

func TestOnlyAFinalSettledReceiptReleasesALease(t *testing.T) {
	root := t.TempDir()
	if leaseReleasable(root, "missing") {
		t.Fatal("a job without a receipt released its lease")
	}
	self, err := currentSupervisor()
	if err != nil {
		t.Fatal(err)
	}
	live := processReceipt{Name: `Local\ZerusScenario-absent`, PID: self.PID, Birth: self.Birth, Accepted: true, Issued: true}
	gone := processReceipt{Name: `Local\ZerusScenario-absent`, PID: self.PID, Birth: self.Birth + 1, Accepted: true, Issued: true}
	for name, c := range map[string]struct {
		state ScenarioState
		want  bool
	}{
		"passed, settled":           {ScenarioState{Status: "passed", Process: gone}, true},
		"cancelled, settled":        {ScenarioState{Status: "cancelled", Process: gone}, true},
		"refused before creation":   {ScenarioState{Status: "not_run"}, true},
		"passed, root alive":        {ScenarioState{Status: "passed", Process: live}, false},
		"unknown":                   {ScenarioState{Status: "unknown", Process: gone}, false},
		"awaiting observation":      {ScenarioState{Status: "awaiting_observation", Process: gone}, false},
		"creation issued only":      {ScenarioState{Status: "creating", Process: processReceipt{Name: live.Name, Issued: true}}, false},
		"issued-only, final status": {ScenarioState{Status: "infrastructure_error", Process: processReceipt{Name: live.Name, Issued: true}}, false},
		"cancel unconfirmed":        {ScenarioState{Status: "cancel_unconfirmed", Process: gone}, false},
	} {
		job := strings.ReplaceAll(name, " ", "-")
		writeJobState(t, root, job, c.state)
		if got := leaseReleasable(root, job); got != c.want {
			t.Errorf("%s: releasable %v, want %v", name, got, c.want)
		}
	}
}

// An unresolved job pins its generation: a later job for another generation
// is held without any delivery; a job for the same generation proceeds.
func TestAnUnresolvedJobPinsItsGeneration(t *testing.T) {
	f := newSharedFixture(t)
	pinned := newJob("pinned")
	expect(t, f.submit("alice-laptop", pinned, "proj-shared"), 200, "pinned job")
	first, _ := f.s.readSubmission(pinned)
	var deliveries []string
	f.s.deliverFn = func(value ExecutorSubmission, offer TreeOffer, kind string) (string, string, error) {
		deliveries = append(deliveries, value.Request.Job+"/"+kind)
		return filepath.Join(f.s.cfg.Root, "fake-"+kind), strings.Repeat("e", 32), nil
	}
	// The pinned job's execution ends without a final receipt: unknown.
	f.s.launchFn = func(string, string) error { return nil }
	f.s.runOne(first)
	if after, _ := f.s.readSubmission(pinned); after.Phase != "unknown" {
		t.Fatalf("pinned job phase %s", after.Phase)
	}
	advance := newJob("advance")
	expect(t, f.submit("bob-laptop", advance, "proj-shared"), 200, "advance job (another artifact generation)")
	second, _ := f.s.readSubmission(advance)
	deliveries = nil
	f.s.runOne(second)
	if after, _ := f.s.readSubmission(advance); after.Phase != "held" || len(deliveries) != 0 {
		t.Fatalf("advance under an unresolved reader: %s, deliveries %v", after.Phase, deliveries)
	}
	// The held job's own lease was not left behind on the artifact slot.
	base := filepath.Join(f.s.cfg.Root, filepath.FromSlash(slotRelative(second, "slots")))
	if _, err := os.Stat(leasePath(base, advance)); !os.IsNotExist(err) {
		t.Fatal("a held job kept a lease")
	}
	// The pinned lease survives a restart.
	restarted := f.service(f.s.cfg)
	restarted.queue = f.s.queue
	restarted.reconcileLeases()
	if _, err := os.Stat(leasePath(base, pinned)); err != nil {
		t.Fatalf("restart released an unresolved lease: %v", err)
	}
	same := newJob("same")
	request := f.request(same, "proj-shared")
	request.Artifact = first.Request.Artifact
	expect(t, f.call("bob-laptop", "POST", "/executor/v1/jobs", request, nil), 200, "same-generation job")
	third, _ := f.s.readSubmission(same)
	deliveries = nil
	f.s.runOne(third)
	if len(deliveries) != 2 {
		t.Fatalf("same-generation job: deliveries %v", deliveries)
	}
}

// Terminating a whole tree is reported settled only at zero processes.
func TestTerminateAndSettleEndsTheWholeTree(t *testing.T) {
	native, log, root, system := nativeFixture(t)
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	env := map[string]string{"SystemRoot": filepath.Dir(system), "ZERUS_TREE_PROBE": "1"}
	if err := native.Start(self, []string{"-test.run=^TestTreeProbeChild$"}, env, root, log, func(processReceipt) error { return nil }); err != nil {
		t.Fatal(err)
	}
	for deadline := time.Now().Add(20 * time.Second); ; time.Sleep(50 * time.Millisecond) {
		if active, _ := jobActive(native.job); active >= 2 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("tree did not start")
		}
	}
	if !native.TerminateAndSettle() {
		t.Fatal("termination not settled")
	}
	if active, err := jobActive(native.job); err != nil || active != 0 {
		t.Fatalf("processes after settlement: %d %v", active, err)
	}
	if settled, err := namedJobSettled(native.info); err != nil || !settled {
		t.Fatalf("named settlement: %v %v", settled, err)
	}
}

// A cancel requested while a job deployed survives a restart.
func TestACancelSurvivesARestart(t *testing.T) {
	f := newSharedFixture(t)
	job := newJob("restart-cancel")
	expect(t, f.submit("alice-laptop", job, "proj-shared"), 200, "submit")
	value, _ := f.s.readSubmission(job)
	_ = f.s.phase(&value, "deploying", "")
	got := f.call("alice-laptop", "POST", "/executor/v1/jobs/"+job+"/cancel", map[string]any{"request_id": "00000000-0000-4000-8000-000000000007"}, nil)
	if got.status != 200 || !strings.Contains(got.body, `"phase":"deploying"`) {
		t.Fatalf("cancel: %d %s", got.status, got.body)
	}
	// The service stops; its in-memory requests are gone.
	cancelRequests.Delete(job)
	restarted := f.service(f.s.cfg)
	restarted.queue = f.s.queue
	restarted.reconcileLeases()
	launched := false
	restarted.deliverFn = func(ExecutorSubmission, TreeOffer, string) (string, string, error) {
		return filepath.Join(f.s.cfg.Root, "fake"), strings.Repeat("e", 32), nil
	}
	restarted.launchFn = func(string, string) error { launched = true; return nil }
	value, _ = restarted.readSubmission(job)
	restarted.runOne(value)
	after, _ := restarted.readSubmission(job)
	if launched || after.Phase != "cancelled" {
		t.Fatalf("after restart: launched=%v phase=%s", launched, after.Phase)
	}
	cancelRequests.Delete(job)
}
