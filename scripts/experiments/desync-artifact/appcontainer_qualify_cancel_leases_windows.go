package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// q7: settled cancellation of a contained tree. A contained probe starts a
// child and sleeps; cancellation terminates the Job and is reported settled
// only when zero processes remain and the named Job reads settled.
func (q *qualifier) q7() error {
	native, err := newNativeScenario(256 * MiB)
	if err != nil {
		return err
	}
	defer native.Close()
	self, _ := os.Executable()
	log, err := os.Create(filepath.Join(q.cfg.Root, "q7.log"))
	if err != nil {
		return err
	}
	defer log.Close()
	spec, _ := json.Marshal([]probeAction{{"spawn", ""}, {"sleep", "120"}})
	env := map[string]string{"SystemRoot": os.Getenv("SystemRoot"), "ZERUS_CONTAINED_PROBE": string(spec)}
	if err := native.StartContained(q.plan, self, []string{"contained-probe"}, env, q.layout.Runtime, log, func(processReceipt) error { return nil }); err != nil {
		return err
	}
	time.Sleep(2 * time.Second)
	if active, err := jobActive(native.job); err != nil || active == 0 {
		return fmt.Errorf("contained tree not running before cancel: %d %v", active, err)
	}
	if !native.TerminateAndSettle() {
		return errors.New("cancellation could not verify settlement")
	}
	if active, err := jobActive(native.job); err != nil || active != 0 {
		return fmt.Errorf("processes after cancel: %d %v", active, err)
	}
	if settled, err := namedJobSettled(native.info); err != nil || !settled {
		return fmt.Errorf("named settlement after cancel: %v %v", settled, err)
	}
	return nil
}

// q9: generation leases on the single installed tree, and project scope of
// the shared generation: same-generation leases share, an advance is held
// while another job's lease names the old generation, release unblocks it;
// a person outside the project (other container, other project capability)
// cannot read the generation.
func (q *qualifier) q9() error {
	relative := "project-slots/proj-shared/slot"
	gen1, gen2 := strings.Repeat("1", 64), strings.Repeat("2", 64)
	if err := acquireLease(q.cfg.Root, relative, slotLease{Generation: gen1, Job: "q9-a"}); err != nil {
		return err
	}
	if err := acquireLease(q.cfg.Root, relative, slotLease{Generation: gen1, Job: "q9-b"}); err != nil {
		return fmt.Errorf("same-generation share: %w", err)
	}
	if err := acquireLease(q.cfg.Root, relative, slotLease{Generation: gen2, Job: "q9-c"}); err == nil || err.Error() != "generation-held" {
		return fmt.Errorf("advance under readers: %v", err)
	}
	if err := leasesAllow(filepath.Join(q.cfg.Root, filepath.FromSlash(relative)), gen2, ""); err == nil {
		return errors.New("tree-receive may advance under a reader")
	}
	for _, job := range []string{"q9-a", "q9-b"} {
		if err := releaseLease(q.cfg.Root, relative, job); err != nil {
			return err
		}
	}
	if err := acquireLease(q.cfg.Root, relative, slotLease{Generation: gen2, Job: "q9-c"}); err != nil {
		return fmt.Errorf("advance after release: %w", err)
	}
	_ = releaseLease(q.cfg.Root, relative, "q9-c")
	return q.contained(q.other, []expectation{
		{probeAction{"read", filepath.Join(q.layout.Generation, "input.txt")}, "deny", true},
		{probeAction{"read", filepath.Join(q.layout.Bundle, "scenario.ps1")}, "deny", true},
	}, nil)
}
