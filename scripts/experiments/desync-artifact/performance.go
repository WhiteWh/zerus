// Bounded executor-host load evidence belongs to the pinned scenario receipt.
package main

import (
	"errors"
	"math"
	"os"
	"path/filepath"
	"time"
)

type PerformancePolicy struct {
	CPU      float64 `json:"cpu_percent"`
	Disk     float64 `json:"disk_percent"`
	DiskBusy float64 `json:"disk_busy_percent"`
	Wait     int     `json:"wait_seconds"`
	Busy     string  `json:"on_busy"`
}
type LoadSample struct {
	CPU      float64 `json:"cpu_percent"`
	Disk     float64 `json:"disk_percent"`
	DiskBusy float64 `json:"disk_busy_percent"`
	At       int64   `json:"at"`
}
type PerformanceReport struct {
	Origin        string            `json:"origin"`
	Scope         string            `json:"scope"`
	Limits        PerformancePolicy `json:"limits"`
	Before        *LoadSample       `json:"before,omitempty"`
	During        *LoadSample       `json:"during,omitempty"`
	After         *LoadSample       `json:"after,omitempty"`
	Samples       uint64            `json:"samples"`
	DuringSamples uint64            `json:"during_samples"`
	WaitedMS      int64             `json:"waited_ms"`
	WaitBudget    int               `json:"effective_wait_seconds"`
	ExecutionMS   int64             `json:"execution_ms"`
	Loaded        bool              `json:"loaded"`
	Valid         bool              `json:"valid"`
	Reason        string            `json:"reason,omitempty"`
}

func validPerformance(p *PerformancePolicy) error {
	if p == nil {
		return nil
	}
	if math.IsNaN(p.CPU) || math.IsInf(p.CPU, 0) || p.CPU < 0 || p.CPU > 100 || math.IsNaN(p.Disk) || math.IsInf(p.Disk, 0) || p.Disk < 0 || p.Disk > 100 || math.IsNaN(p.DiskBusy) || math.IsInf(p.DiskBusy, 0) || p.DiskBusy < 0 || p.DiskBusy > 100 || p.Wait < 0 || p.Wait > 3600 || (p.Busy != "not_run" && p.Busy != "run_loaded") {
		return errors.New("invalid performance admission policy")
	}
	return nil
}
func validLoad(s LoadSample) bool {
	return !math.IsNaN(s.CPU) && !math.IsNaN(s.Disk) && !math.IsNaN(s.DiskBusy) && !math.IsInf(s.DiskBusy, 0) && s.DiskBusy >= 0 && s.DiskBusy <= 100 && !math.IsInf(s.CPU, 0) && !math.IsInf(s.Disk, 0) && s.CPU >= 0 && s.CPU <= 100 && s.Disk >= 0 && s.Disk <= 100
}
func quietLoad(s LoadSample, p PerformancePolicy) bool {
	return validLoad(s) && s.CPU <= p.CPU && s.Disk <= p.Disk && s.DiskBusy <= p.DiskBusy
}

type loadSource interface {
	Sample() (LoadSample, error)
	Close()
}
type performanceMonitor struct {
	source loadSource
	report *PerformanceReport
	last   time.Time
	now    func() time.Time
	sleep  func(time.Duration)
}

func (m *performanceMonitor) sample() (*LoadSample, error) {
	sample, err := m.source.Sample()
	m.last = m.now()
	if err != nil || !validLoad(sample) {
		m.report.Valid = false
		m.report.Reason = "executor host counters unavailable or invalid"
		return nil, errors.New(m.report.Reason)
	}
	sample.At = m.now().UnixMilli()
	m.report.Samples++
	return &sample, nil
}
func (m *performanceMonitor) gate(checkpoint func() error) error {
	began := m.now()
	deadline := began.Add(time.Duration(m.report.WaitBudget+2) * time.Second)
	consecutive := 0
	defer func() { m.report.WaitedMS = m.now().Sub(began).Milliseconds() }()
	for {
		if err := checkpoint(); err != nil {
			return err
		}
		m.sleep(time.Second)
		sample, err := m.sample()
		if err != nil {
			return err
		}
		m.report.Before = sample
		if quietLoad(*sample, m.report.Limits) {
			consecutive++
		} else {
			consecutive = 0
		}
		if consecutive >= 2 {
			return nil
		}
		if !m.now().Before(deadline) {
			if m.report.Limits.Busy == "run_loaded" {
				m.report.Loaded = true
				return nil
			}
			m.report.Reason = "executor host stayed busy; scenario was not run"
			return errors.New(m.report.Reason)
		}
	}
}
func (m *performanceMonitor) poll() {
	if m == nil || m.now().Sub(m.last) < time.Second {
		return
	}
	sample, err := m.sample()
	if err != nil {
		return
	}
	m.report.DuringSamples++
	if m.report.During == nil {
		copy := *sample
		m.report.During = &copy
	} else {
		m.report.During.CPU = max(m.report.During.CPU, sample.CPU)
		m.report.During.Disk = max(m.report.During.Disk, sample.Disk)
		m.report.During.DiskBusy = max(m.report.During.DiskBusy, sample.DiskBusy)
		m.report.During.At = sample.At
	}
	if !quietLoad(*sample, m.report.Limits) {
		m.report.Loaded = true
	}
}
func (m *performanceMonitor) finish(elapsed time.Duration) {
	if m == nil {
		return
	}
	m.report.ExecutionMS = elapsed.Milliseconds()
	if remaining := time.Second - m.now().Sub(m.last); remaining > 0 {
		m.sleep(remaining)
	}
	if sample, err := m.sample(); err == nil {
		m.report.After = sample
		if !quietLoad(*sample, m.report.Limits) {
			m.report.Loaded = true
		}
	}
}
func beginPerformance(cfg ScenarioConfig, state *ScenarioState, timeout int) (*performanceMonitor, error) {
	if cfg.Performance == nil {
		return nil, nil
	}
	p := *cfg.Performance
	state.Performance = &PerformanceReport{Origin: "executor-host", Scope: "host-cpu-volumes-physical-disks", Limits: p, WaitBudget: p.Wait, Valid: true}
	if err := validPerformance(&p); err != nil {
		state.Performance.Valid = false
		state.Performance.Reason = "invalid performance policy"
		return nil, err
	}
	if cfg.Device != "host" {
		state.Performance.Valid = false
		state.Performance.Reason = "host load cannot qualify a console device"
		return nil, errors.New(state.Performance.Reason)
	}
	if cfg.MaxSeconds > 0 {
		available := cfg.MaxSeconds - timeout - 2
		if available < 0 {
			state.Performance.Valid = false
			state.Performance.Reason = "executor budget does not admit load sampling"
			return nil, errors.New(state.Performance.Reason)
		}
		state.Performance.WaitBudget = min(p.Wait, available)
	}
	source, err := newLoadSource(cfg.ExecutorRoot, cfg.ArtifactSlot, cfg.ScenarioSlot)
	if err != nil {
		state.Performance.Valid = false
		state.Performance.Reason = "executor host counters unavailable"
		return nil, errors.New(state.Performance.Reason)
	}
	return &performanceMonitor{source: source, report: state.Performance, last: time.Now(), now: time.Now, sleep: time.Sleep}, nil
}
func finishPerformanceRefusal(work string, cfg ScenarioConfig, state *ScenarioState, log *os.File, reason string) error {
	state.Status = "not_run"
	state.Reason = reason
	state.Finished = time.Now().UnixMilli()
	if err := log.Sync(); err != nil {
		return err
	}
	if err := log.Close(); err != nil {
		return err
	}
	var err error
	state.LogSHA256, err = hashFile(filepath.Join(work, "stdout.log"))
	if err != nil {
		return err
	}
	state.Evidence = []EvidenceFile{}
	state.Cleanup, err = generatedObjects(work)
	if err != nil {
		return err
	}
	state.ResultHash = resultDigest(*state)
	return saveScenario(work, state)
}
