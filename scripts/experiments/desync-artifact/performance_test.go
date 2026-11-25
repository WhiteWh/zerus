package main

import (
	"errors"
	"math"
	"testing"
	"time"
)

type fakeLoadSource struct {
	samples  []LoadSample
	position int
	bad      bool
}

func (f *fakeLoadSource) Close() {}
func (f *fakeLoadSource) Sample() (LoadSample, error) {
	if f.bad {
		return LoadSample{}, errors.New("synthetic unavailable counter")
	}
	s := f.samples[min(f.position, len(f.samples)-1)]
	f.position++
	return s, nil
}
func TestPerformanceAdmissionAndUnavailableEvidence(t *testing.T) {
	quiet := LoadSample{CPU: 10, Disk: 40, DiskBusy: 5}
	busy := LoadSample{CPU: 80, Disk: 40, DiskBusy: 50}
	for _, row := range []struct {
		name, mode      string
		samples         []LoadSample
		wait            int
		allowed, loaded bool
	}{{"quiet", "not_run", []LoadSample{quiet}, 0, true, false}, {"busy-refusal", "not_run", []LoadSample{busy}, 0, false, false}, {"explicit-loaded", "run_loaded", []LoadSample{busy}, 0, true, true}, {"settles", "not_run", []LoadSample{busy, quiet, quiet}, 2, true, false}, {"reset-window", "not_run", []LoadSample{quiet, busy, quiet, quiet}, 2, true, false}} {
		t.Run(row.name, func(t *testing.T) {
			clock := time.Unix(0, 0)
			source := &fakeLoadSource{samples: row.samples}
			report := &PerformanceReport{Limits: PerformancePolicy{CPU: 20, Disk: 90, DiskBusy: 20, Wait: row.wait, Busy: row.mode}, WaitBudget: row.wait, Valid: true}
			m := &performanceMonitor{source: source, report: report, last: clock, now: func() time.Time { return clock }, sleep: func(d time.Duration) { clock = clock.Add(d) }}
			if (m.gate(func() error { return nil }) == nil) != row.allowed || report.Loaded != row.loaded {
				t.Fatal("synthetic gate verdict differs")
			}
			if report.WaitedMS > int64(row.wait+2)*1000 {
				t.Fatal("gate exceeded bounded window")
			}
			if row.allowed {
				clock = clock.Add(time.Second)
				m.poll()
				if report.During == nil || report.DuringSamples != 1 {
					t.Fatal("missing measured running sample")
				}
				source.bad = true
				clock = clock.Add(time.Second)
				m.poll()
				if report.Valid {
					t.Fatal("missing counters were treated as valid zero load")
				}
			}
		})
	}
	if validLoad(LoadSample{CPU: math.NaN()}) || validLoad(LoadSample{DiskBusy: math.Inf(1)}) {
		t.Fatal("non-finite load accepted")
	}
	p := &PerformancePolicy{CPU: 50, Disk: 90, DiskBusy: 20, Wait: 0, Busy: "not_run"}
	state := ScenarioState{}
	if _, err := beginPerformance(ScenarioConfig{Performance: p, Device: "console"}, &state, 1); err == nil || state.Performance.Valid {
		t.Fatal("host counters qualified a console device")
	}
}
func TestNativeLocalLoadCounters(t *testing.T) {
	dir := t.TempDir()
	source, err := newLoadSource(dir, dir, dir)
	if err != nil {
		t.Fatal("native local load counters unavailable")
	}
	defer source.Close()
	time.Sleep(time.Second)
	sample, err := source.Sample()
	if err != nil || !validLoad(sample) {
		t.Fatal("native local load sample invalid")
	}
	// No host values, names, paths or measurements are printed by this test.
}
