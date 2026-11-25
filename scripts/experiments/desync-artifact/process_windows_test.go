package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestScenarioNativeCreationAndChildSettlement(t *testing.T) {
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
	accepted := false
	if err := native.Start(filepath.Join(system, "cmd.exe"), []string{"/d", "/c", "echo synthetic-process-contract"}, map[string]string{"SystemRoot": filepath.Dir(system)}, root, log, func(info processReceipt) error {
		accepted = info.Accepted && info.PID != 0 && info.Birth != 0
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	code, timedOut, err := native.Wait(10, nil)
	if err != nil || timedOut || code != 0 || !accepted {
		t.Fatal("native process admission or settlement failed")
	}
	if settled, err := namedJobSettled(native.info); err != nil || !settled {
		t.Fatal("owned job was not settled")
	}
}
