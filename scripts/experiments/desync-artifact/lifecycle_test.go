package main

import (
	"encoding/json"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

func TestLifecycleCrashChild(t *testing.T) {
	p := os.Getenv("ZERUS_SYNTHETIC_CRASH_CONFIG")
	if p == "" {
		return
	}
	if err := run("apply", p); err != nil {
		os.Exit(3)
	}
	os.Exit(4) // The injected abrupt exit must happen before returning here.
}

func TestAbruptExitReleasesCustodyAndRepairsActualBytes(t *testing.T) {
	p, cfg, m, digest := prepared(t, "all_changed")
	cfg.CrashAfter = 2
	writeConfig(t, p, cfg)
	child := exec.Command(os.Args[0], "-test.run=^TestLifecycleCrashChild$")
	child.Env = append(os.Environ(), "ZERUS_SYNTHETIC_CRASH_CONFIG="+p)
	if err := child.Run(); err == nil || child.ProcessState.ExitCode() != 86 {
		t.Fatal("expected abrupt helper exit did not occur")
	}
	s, err := openSlot(filepath.Dir(p))
	if err != nil {
		t.Fatal("crash did not release custody")
	}
	r, err := s.receipt(m, digest)
	s.Close()
	if err != nil || r.State != "applying" {
		t.Fatal("crash left an invalid readiness receipt")
	}
	if err := run("status", p); err == nil {
		t.Fatal("crashed update was ready")
	}
	cfg.CrashAfter = 0
	writeConfig(t, p, cfg)
	if err := run("apply", p); err != nil {
		t.Fatal(err)
	}
	assertReady(t, p)
}

func TestCrashAtDurabilityBoundariesDoesNotPublishPrematureReadiness(t *testing.T) {
	for _, phase := range []string{"before_apply", "after_apply", "after_flush", "before_ready", "after_ready"} {
		t.Run(phase, func(t *testing.T) {
			p, cfg, m, digest := prepared(t, "insert")
			cfg.CrashPhase = phase
			writeConfig(t, p, cfg)
			child := exec.Command(os.Args[0], "-test.run=^TestLifecycleCrashChild$")
			child.Env = append(os.Environ(), "ZERUS_SYNTHETIC_CRASH_CONFIG="+p)
			if err := child.Run(); err == nil || child.ProcessState.ExitCode() != 86 {
				t.Fatal("boundary crash did not occur")
			}
			s, err := openSlot(filepath.Dir(p))
			if err != nil {
				t.Fatal(err)
			}
			r, err := s.receipt(m, digest)
			s.Close()
			if err != nil {
				t.Fatal(err)
			}
			if phase == "after_ready" {
				if r.State != "ready" || run("status", p) != nil {
					t.Fatal("durably verified output was not ready")
				}
			} else if r.State == "ready" || run("status", p) == nil {
				t.Fatal("premature readiness after crash")
			}
			cfg.CrashPhase = ""
			writeConfig(t, p, cfg)
			if err := run("apply", p); err != nil {
				t.Fatal(err)
			}
			assertReady(t, p)
		})
	}
}

func TestInterruptedPreparationReconcilesCompletePublication(t *testing.T) {
	p, _, original, digest := prepared(t, "same")
	if err := os.Remove(filepath.Join(filepath.Dir(p), "state.json")); err != nil {
		t.Fatal(err)
	}
	if err := run("apply", p); err == nil {
		t.Fatal("missing initial receipt admitted an update")
	}
	if err := run("prepare", p); err != nil {
		t.Fatal(err)
	}
	s, err := openSlot(filepath.Dir(p))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	m, now, err := s.manifest()
	if err != nil || m.DeliveryID != original.DeliveryID || now != digest {
		t.Fatal("preparation recovery rebound publication")
	}
	r, err := s.receipt(m, now)
	if err != nil || r.State != "prepared" {
		t.Fatal("initial receipt was not recovered")
	}
}

func prepared(t *testing.T, scenario string) (string, Config, Manifest, string) {
	t.Helper()
	p, cfg := fixture(t, scenario)
	if err := run("prepare", p); err != nil {
		t.Fatal(err)
	}
	s, err := openSlot(filepath.Dir(p))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	m, digest, err := s.manifest()
	if err != nil {
		t.Fatal(err)
	}
	return p, cfg, m, digest
}

func TestRepeatedPreparationDoesNotReseedOrRetarget(t *testing.T) {
	p, cfg, original, digest := prepared(t, "all_changed")
	if err := run("prepare", p); err != nil {
		t.Fatal(err)
	}
	cfg.FailAfter = 2
	writeConfig(t, p, cfg)
	if err := run("apply", p); err == nil {
		t.Fatal("failure was not injected")
	}
	target := filepath.Join(filepath.Dir(p), "receiver.bin")
	before, err := hashFile(target)
	if err != nil {
		t.Fatal(err)
	}
	if err := run("prepare", p); err != nil {
		t.Fatal(err)
	}
	after, err := hashFile(target)
	if err != nil || after != before {
		t.Fatal("duplicate preparation changed mixed contents")
	}
	s, err := openSlot(filepath.Dir(p))
	if err != nil {
		t.Fatal(err)
	}
	m, now, err := s.manifest()
	s.Close()
	if err != nil || now != digest || m.DeliveryID != original.DeliveryID {
		t.Fatal("duplicate request identity changed")
	}
	cfg.Scenario = "same"
	writeConfig(t, p, cfg)
	if err := run("prepare", p); err == nil {
		t.Fatal("existing request was retargeted")
	}
	if err := run("apply", p); err == nil {
		t.Fatal("changed request was applied")
	}
}

func TestNativeSourceLeaseExcludesWritersAndReplacement(t *testing.T) {
	p, cfg := fixture(t, "insert")
	lease, err := openSealed(cfg.Source, "", false)
	if err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(cfg.Source, os.O_WRONLY, 0600)
	if err == nil {
		f.Close()
		t.Fatal("writer admitted under source lease")
	}
	if err := os.Rename(cfg.Source, cfg.Source+".moved"); err == nil {
		t.Fatal("leased source was replaced")
	}
	lease.Close()
	writer, err := os.OpenFile(cfg.Source, os.O_WRONLY, 0600)
	if err != nil {
		t.Fatal(err)
	}
	if err := run("prepare", p); err == nil {
		writer.Close()
		t.Fatal("existing writer was ignored")
	}
	writer.Close()
	if err := run("prepare", p); err != nil {
		t.Fatal(err)
	}
}

func TestSlotCustodyExcludesConcurrentDeliveryAndAncestorRename(t *testing.T) {
	p, _ := fixture(t, "same")
	s, err := openSlot(filepath.Dir(p))
	if err != nil {
		t.Fatal(err)
	}
	if second, err := openSlot(filepath.Dir(p)); err == nil {
		second.Close()
		s.Close()
		t.Fatal("second slot owner admitted")
	}
	if err := run("prepare", p); err == nil {
		s.Close()
		t.Fatal("concurrent delivery admitted")
	}
	parent := filepath.Dir(filepath.Dir(p))
	if err := os.Rename(parent, parent+".moved"); err == nil {
		s.Close()
		t.Fatal("held ancestor renamed")
	}
	s.Close()
	if err := run("prepare", p); err != nil {
		t.Fatal(err)
	}
}

func TestReceiptsAndManifestsCannotBeRebound(t *testing.T) {
	for _, kind := range []string{"protocol", "backend", "artifact", "receipt", "missing_receipt", "index"} {
		t.Run(kind, func(t *testing.T) {
			p, _, m, digest := prepared(t, "insert")
			work := filepath.Dir(p)
			target := filepath.Join(work, "receiver.bin")
			before, _ := hashFile(target)
			switch kind {
			case "protocol":
				m.Protocol++
				durableJSON(filepath.Join(work, "manifest.json"), m)
			case "backend":
				m.Artifact.Backend = "unsupported"
				durableJSON(filepath.Join(work, "manifest.json"), m)
			case "artifact":
				m.ArtifactID = sumBytes([]byte("different artifact"))
				durableJSON(filepath.Join(work, "manifest.json"), m)
			case "receipt":
				r := receiptFor(m, digest)
				r.Sequence = 2
				r.State = "ready"
				r.DeliveryID = "00000000000000000000000000000000"
				durableJSON(filepath.Join(work, "state.json"), r)
			case "missing_receipt":
				os.Remove(filepath.Join(work, "state.json"))
			case "index":
				f, err := os.OpenFile(filepath.Join(work, "target.caibx"), os.O_WRONLY, 0600)
				if err != nil {
					t.Fatal(err)
				}
				f.WriteAt([]byte{0, 0, 0, 0}, 0)
				f.Close()
			}
			if err := run("apply", p); err == nil {
				t.Fatal("mismatched record was accepted")
			}
			after, err := hashFile(target)
			if err != nil || before != after {
				t.Fatal("refused admission modified receiver")
			}
		})
	}
}

func TestSourceChangesCannotHideBehindMetadata(t *testing.T) {
	for _, replace := range []bool{false, true} {
		p, cfg, _, _ := prepared(t, "same")
		info, err := os.Stat(cfg.Source)
		if err != nil {
			t.Fatal(err)
		}
		if replace {
			if err := os.Rename(cfg.Source, cfg.Source+".old"); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(cfg.Source, make([]byte, cfg.Length), 0600); err != nil {
				t.Fatal(err)
			}
		} else {
			f, err := os.OpenFile(cfg.Source, os.O_WRONLY, 0600)
			if err != nil {
				t.Fatal(err)
			}
			f.WriteAt([]byte("synthetic edit"), 4)
			f.Close()
		}
		if err := os.Chtimes(cfg.Source, info.ModTime(), info.ModTime()); err != nil {
			t.Fatal(err)
		}
		if err := run("apply", p); err == nil {
			t.Fatal("changed publication with restored timestamp accepted")
		}
	}
}

func TestReadyAdmissionVerifiesBytesAndKeepsCustody(t *testing.T) {
	p, cfg, m, digest := prepared(t, "insert")
	if err := run("status", p); err == nil {
		t.Fatal("prepared was ready")
	}
	if err := run("apply", p); err != nil {
		t.Fatal(err)
	}
	// The publisher is no longer needed to admit a completely verified build.
	if err := os.Remove(cfg.Source); err != nil {
		t.Fatal(err)
	}
	if err := run("status", p); err != nil {
		t.Fatal(err)
	}
	s, err := openSlot(filepath.Dir(p))
	if err != nil {
		t.Fatal(err)
	}
	f, err := s.ready(m, digest)
	if err != nil {
		s.Close()
		t.Fatal(err)
	}
	target := filepath.Join(filepath.Dir(p), "receiver.bin")
	w, err := os.OpenFile(target, os.O_WRONLY, 0600)
	if err == nil {
		w.Close()
		f.Close()
		s.Close()
		t.Fatal("verified receiver was writable during admission")
	}
	if err := run("apply", p); err == nil {
		f.Close()
		s.Close()
		t.Fatal("test custody allowed an update")
	}
	f.Close()
	s.Close()
	w, err = os.OpenFile(target, os.O_WRONLY, 0600)
	if err != nil {
		t.Fatal(err)
	}
	w.WriteAt([]byte("synthetic corruption"), 9)
	w.Close()
	if err := run("status", p); err == nil {
		t.Fatal("ready label hid later corruption")
	}
	var receipt Receipt
	if _, err := readControl(filepath.Join(filepath.Dir(p), "state.json"), &receipt); err != nil || receipt.State == "ready" {
		t.Fatal("detected corruption was not invalidated")
	}
}

func TestCopiedSlotCannotClaimDestinationIdentity(t *testing.T) {
	p, _, _, _ := prepared(t, "same")
	work := t.TempDir()
	for _, name := range []string{"config.json", "manifest.json", "state.json", "target.caibx", "receiver.bin"} {
		b, err := os.ReadFile(filepath.Join(filepath.Dir(p), name))
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(work, name), b, 0600); err != nil {
			t.Fatal(err)
		}
	}
	if err := run("apply", filepath.Join(work, "config.json")); err == nil {
		t.Fatal("copied destination was accepted")
	}
}

func TestReceiptTransitionsAndUnsafeStagingFailClosed(t *testing.T) {
	p, _, m, digest := prepared(t, "same")
	s, err := openSlot(filepath.Dir(p))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	r, err := s.receipt(m, digest)
	if err != nil {
		t.Fatal(err)
	}
	if err := s.transition(&r, "ready"); err == nil {
		t.Fatal("verification was bypassed")
	}
	before := r
	if err := os.Link(filepath.Join(s.work, "receiver.bin"), filepath.Join(s.work, "state.json.next")); err != nil {
		t.Fatal(err)
	}
	if err := s.transition(&r, "applying"); err == nil {
		t.Fatal("hard-linked receipt staging was accepted")
	}
	if r != before {
		t.Fatal("failed durable transition advanced in-memory receipt")
	}
	var persisted Receipt
	b, err := os.ReadFile(filepath.Join(s.work, "state.json"))
	if err != nil || json.Unmarshal(b, &persisted) != nil || persisted != before {
		t.Fatal("failed transition changed durable readiness")
	}
}
