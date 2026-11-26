package main

// The AppContainer backend: activation from a trusted qualification record,
// the runtime probe, and the container specification a shared job carries
// into its scenario. Activation is fail-closed: without a record that the
// installer protected, bound to this exact binary, matrix and protocol, with
// every case passed by the distributable itself, the backend stays
// unregistered and multi-person mode refuses to start.

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"golang.org/x/sys/windows"
)

const (
	qualificationProtocol = 1
	qualificationSidecar  = "appcontainer-qualification.json"
	runtimeProbeLifetime  = 10 * time.Minute
)

// requiredQualificationCases must all pass in a distributable record.
var requiredQualificationCases = []string{"Q0", "Q1", "Q2", "Q3", "Q4", "Q5", "Q6", "Q7", "Q8", "Q9", "Q10"}

type qualificationCase struct {
	Case    string `json:"case"`
	Verdict string `json:"verdict"` // pass, fail, unsupported
	Detail  string `json:"detail,omitempty"`
}

// qualificationRecord is written by `hgs-executor appcontainer-qualify` on the
// disposable runner and placed beside the binary by trusted packaging.
type qualificationRecord struct {
	Protocol       int                 `json:"protocol"`
	Evidence       string              `json:"evidence"` // "distributable"; harness output is "harness"
	Backend        string              `json:"backend"`
	BinarySHA256   string              `json:"binary_sha256"`
	SourceIdentity string              `json:"source_identity"`
	MatrixDigest   string              `json:"matrix_digest"`
	ConfigIdentity string              `json:"config_identity"`
	OSVersion      string              `json:"os_version"`
	Runner         string              `json:"runner"`
	ServiceAdmin   bool                `json:"service_admin"`
	Cases          []qualificationCase `json:"cases"`
}

// matrixDigest identifies the grant matrix a record qualified.
func matrixDigest() string {
	data, _ := json.Marshal(appContainerGrants)
	return sumBytes(data)
}

// containerConfigIdentity identifies the effective backend configuration:
// matrix, LPAC (off) and network (none).
func containerConfigIdentity() string {
	return sumBytes([]byte(appContainerBackend + "\n" + matrixDigest() + "\nlpac=false\nnetwork=none\n"))
}

// ownBinarySHA256 hashes this executable through one handle that denies
// writers while it is read.
func ownBinarySHA256() (string, error) {
	path, err := os.Executable()
	if err != nil {
		return "", err
	}
	f, err := nativeOpen(path, windows.GENERIC_READ, windows.FILE_SHARE_READ, windows.OPEN_EXISTING, 0)
	if err != nil {
		return "", err
	}
	defer f.Close()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

// Host compatibility rule: a record qualified on the disposable windows-2025
// runner (Windows 10.0, build 26100 or later, the 24H2 kernel generation)
// covers hosts of the same major/minor version with a build no older than
// minimumQualifiedBuild. Older hosts are not covered by that evidence.
const minimumQualifiedBuild = 26100

// parseOSVersion reads "major.minor.build".
func parseOSVersion(s string) (major, minor, build int, ok bool) {
	parts := strings.Split(s, ".")
	if len(parts) != 3 {
		return 0, 0, 0, false
	}
	var values [3]int
	for i, p := range parts {
		n, err := strconv.Atoi(p)
		if err != nil || n < 0 || strconv.Itoa(n) != p {
			return 0, 0, 0, false
		}
		values[i] = n
	}
	return values[0], values[1], values[2], true
}

// provenanceAllowed checks the record's runner and OS provenance and this
// host's compatibility with it. The runner string is asserted by the CI job
// that ran the distributable, not signed; what vouches for it is the trusted
// packaging step that copies only the record produced by the reviewed
// workflow for the same commit and binary, and the installer that protects
// the file (activateAppContainer reads it with installer trust).
func provenanceAllowed(r qualificationRecord, host string) error {
	image, version, ok := strings.Cut(r.Runner, " ")
	if !ok || !strings.HasPrefix(image, "win25") || version == "" || strings.ContainsAny(version, " \n") {
		return accessCode("qualification-runner")
	}
	major, minor, build, ok := parseOSVersion(r.OSVersion)
	if !ok || major != 10 || minor != 0 || build < minimumQualifiedBuild {
		return accessCode("qualification-os")
	}
	hMajor, hMinor, hBuild, ok := parseOSVersion(host)
	if !ok || hMajor != major || hMinor != minor || hBuild < minimumQualifiedBuild {
		return accessCode("qualification-host-incompatible")
	}
	return nil
}

// checkQualificationRecord is the activation decision on a parsed record.
func checkQualificationRecord(r qualificationRecord, binary string) error {
	return checkQualificationRecordOn(r, binary, executorSourceIdentity, osVersion())
}

func checkQualificationRecordOn(r qualificationRecord, binary, source, host string) error {
	switch {
	case r.Protocol != qualificationProtocol:
		return accessCode("qualification-protocol")
	case r.Evidence != "distributable":
		return accessCode("qualification-not-distributable")
	case r.Backend != appContainerBackend:
		return accessCode("qualification-backend")
	case !isHex64(r.BinarySHA256) || r.BinarySHA256 != binary:
		return accessCode("qualification-other-binary")
	case r.MatrixDigest != matrixDigest():
		return accessCode("qualification-other-matrix")
	case r.ConfigIdentity != containerConfigIdentity():
		return accessCode("qualification-other-config")
	case r.ServiceAdmin:
		return accessCode("qualification-admin-service")
	case !isHex64(source) || r.SourceIdentity != source:
		// Only a packaged build embeds its source identity; a development
		// build ("unversioned") can never activate.
		return accessCode("qualification-other-source")
	}
	if err := provenanceAllowed(r, host); err != nil {
		return err
	}
	required := map[string]bool{}
	for _, name := range requiredQualificationCases {
		required[name] = true
	}
	verdicts := map[string]string{}
	for _, c := range r.Cases {
		if !required[c.Case] {
			return accessCode("qualification-unexpected-case")
		}
		if _, dup := verdicts[c.Case]; dup {
			return accessCode("qualification-duplicate-case")
		}
		verdicts[c.Case] = c.Verdict
	}
	for _, name := range requiredQualificationCases {
		if verdicts[name] != "pass" {
			return accessCode("qualification-incomplete")
		}
	}
	return nil
}

// activateAppContainer registers the backend only for a trusted, matching,
// complete record beside this binary. The record is read through the same
// held-handle, installer-trust check as the bootstrap configuration, so a
// user-editable file never enables isolation.
func activateAppContainer(bootstrap policyTrust) error {
	exe, err := os.Executable()
	if err != nil {
		return err
	}
	var record qualificationRecord
	if err := loadSharedConfig(filepath.Join(filepath.Dir(exe), qualificationSidecar), bootstrap, &record); err != nil {
		return accessCode("qualification-unavailable")
	}
	binary, err := ownBinarySHA256()
	if err != nil {
		return err
	}
	if err := checkQualificationRecord(record, binary); err != nil {
		return err
	}
	isolationBackends[appContainerBackend] = &appContainerIsolation{record: record, binary: binary, api: windowsProfiles{}, probes: map[string]time.Time{}}
	return nil
}

// appContainerIsolation is the registered backend.
type appContainerIsolation struct {
	record qualificationRecord
	binary string
	api    profileAPI
	mu     sync.Mutex           // guards probes
	flight sync.Mutex           // serializes runtime probes (single flight)
	probes map[string]time.Time // key -> expiry, at most maxProbeCache entries
	probe  func(root string, plan containerPlan) error // runtimeProbe; tests substitute
}

const maxProbeCache = 256

func (b *appContainerIsolation) Qualified() error {
	if b == nil || b.binary == "" {
		return accessCode("isolation-unavailable")
	}
	return nil
}

// containerSpec is the container a shared job's scenario must use, as
// written in its run configuration.
type containerSpec struct {
	Name            string   `json:"name"`
	SID             string   `json:"sid"`
	Capabilities    []string `json:"capabilities"`
	CapabilityNames []string `json:"capability_names"`
}

func specOf(plan containerPlan) *containerSpec {
	spec := &containerSpec{Name: plan.Name, SID: plan.SID.String(), CapabilityNames: append([]string{}, plan.CapabilityNames...)}
	for _, c := range plan.Capabilities {
		spec.Capabilities = append(spec.Capabilities, c.String())
	}
	return spec
}

// planOf re-derives the plan from a stored spec; every SID must be the
// derivation of its name, so an edited run configuration cannot name another
// container or capability.
func planOf(spec *containerSpec) (containerPlan, error) {
	if spec == nil || !strings.HasPrefix(spec.Name, "Zerus.AC.") || len(spec.Capabilities) != len(spec.CapabilityNames) || len(spec.Capabilities) > 4 {
		return containerPlan{}, accessCode("container-spec-invalid")
	}
	sid, err := appContainerSID(spec.Name)
	if err != nil || sid.String() != spec.SID {
		return containerPlan{}, accessCode("container-spec-invalid")
	}
	plan := containerPlan{Name: spec.Name, SID: sid}
	for i, name := range spec.CapabilityNames {
		if !strings.HasPrefix(name, "zerusProject") {
			return containerPlan{}, accessCode("container-spec-invalid")
		}
		c, err := capabilitySID(name)
		if err != nil || c.String() != spec.Capabilities[i] {
			return containerPlan{}, accessCode("container-spec-invalid")
		}
		plan.Capabilities = append(plan.Capabilities, c)
		plan.CapabilityNames = append(plan.CapabilityNames, name)
	}
	return plan, nil
}

// probeKey binds a runtime probe result to the binary, the container and its
// capabilities, the effective configuration and the policy trust identity.
func (b *appContainerIsolation) probeKey(plan containerPlan, trust string) string {
	caps := append([]string{}, plan.CapabilityNames...)
	sort.Strings(caps)
	return sumBytes([]byte(b.binary + "\n" + plan.Name + "\n" + strings.Join(caps, ",") + "\n" + containerConfigIdentity() + "\n" + trust))
}

// QualifiedFor runs the bounded runtime probe for a container, or reuses a
// fresh result for the same key. It establishes that this host still
// behaves as qualified for this container; it is not the full suite and
// never registers a backend.
func (b *appContainerIsolation) QualifiedFor(root string, plan containerPlan, origin containerOrigin, trust string) error {
	key := b.probeKey(plan, trust)
	fresh := func() bool {
		b.mu.Lock()
		defer b.mu.Unlock()
		until, ok := b.probes[key]
		return ok && time.Now().Before(until)
	}
	if fresh() {
		return nil
	}
	// One probe at a time: concurrent misses wait, then reuse the result
	// the first one recorded instead of rebuilding anything.
	b.flight.Lock()
	defer b.flight.Unlock()
	if fresh() {
		return nil
	}
	if err := ensureContainerProfile(b.api, root, plan, origin); err != nil {
		return err
	}
	probe := b.probe
	if probe == nil {
		probe = runtimeProbe
	}
	if err := probe(root, plan); err != nil {
		return err
	}
	b.mu.Lock()
	now := time.Now()
	for k, until := range b.probes {
		if !now.Before(until) {
			delete(b.probes, k)
		}
	}
	for len(b.probes) >= maxProbeCache {
		oldest, at := "", now.Add(runtimeProbeLifetime)
		for k, until := range b.probes {
			if until.Before(at) {
				oldest, at = k, until
			}
		}
		delete(b.probes, oldest)
	}
	b.probes[key] = now.Add(runtimeProbeLifetime)
	b.mu.Unlock()
	return nil
}

// runtimeProbe launches the contained probe with the real plan: the token is
// verified before resume (StartContained); a protected sentinel the service
// can read (positive control) must be denied with ERROR_ACCESS_DENIED, the
// service process must not open, and the probe's own runtime folder must be
// writable.
func runtimeProbe(root string, plan containerPlan) error {
	// A unique, exact-owned probe root under held parents: never another
	// probe's files. It is removed only after this probe's tree settled.
	id, err := newIdentity()
	if err != nil {
		return err
	}
	relative := "appcontainers/probes/" + id[:32]
	parents, err := managedDirectory(root, relative)
	defer closeFiles(parents)
	if err != nil {
		return err
	}
	base := filepath.Join(root, filepath.FromSlash(relative))
	settled := false
	defer func() {
		if settled {
			_ = os.RemoveAll(base)
		}
	}()
	for _, dir := range []string{"protected", "runtime", "outputs"} {
		if err := os.Mkdir(filepath.Join(base, dir), 0700); err != nil {
			return err
		}
	}
	service, err := serviceSID()
	if err != nil {
		return err
	}
	protected := filepath.Join(base, "protected")
	if err := applyProtectedFolder(protected, "D:P(A;OICI;FA;;;"+service.String()+")(A;OICI;FA;;;SY)"); err != nil {
		return err
	}
	sentinel := filepath.Join(protected, "sentinel.txt")
	if err := os.WriteFile(sentinel, []byte("synthetic probe sentinel"), 0600); err != nil {
		return err
	}
	if control := runProbeAction(probeAction{"read", sentinel}); !control.OK {
		return accessCode("probe-control-failed")
	}
	g, err := grantFor(roleJobHome, principalPackage)
	if err != nil {
		return err
	}
	sddl, err := privateFolderSDDL(service, g, plan)
	if err != nil {
		return err
	}
	if err := applyPrivateFolder(filepath.Join(base, "runtime"), sddl); err != nil {
		return err
	}
	if err := addTraverse(base, plan); err != nil {
		return err
	}
	actions := []probeAction{{"read", sentinel}, {"openprocess", strconv.Itoa(os.Getpid())}, {"write", filepath.Join(base, "runtime", "probe.txt")}}
	outcomes, err := runContainedProbe(plan, actions, filepath.Join(base, "runtime"), nil, 60)
	if err != nil {
		return err
	}
	settled = true // runContainedProbe returned only after the Job reached zero
	if !outcomes[actions[0].Op+" "+actions[0].Target].denied() || !outcomes[actions[1].Op+" "+actions[1].Target].denied() ||
		!outcomes[actions[2].Op+" "+actions[2].Target].OK {
		return accessCode("probe-boundary-failed")
	}
	return nil
}

// applyProtectedFolder sets a protected DACL with no contained entry on an
// empty folder and reads it back.
func applyProtectedFolder(path, sddl string) error {
	f, err := nativeOpen(path, windows.READ_CONTROL|windows.WRITE_DAC|windows.FILE_LIST_DIRECTORY, windows.FILE_SHARE_READ, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS)
	if err != nil {
		return err
	}
	defer f.Close()
	sd, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		return err
	}
	dacl, _, _ := sd.DACL()
	if err := windows.SetSecurityInfo(windows.Handle(f.Fd()), windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION, nil, nil, dacl, nil); err != nil {
		return accessCode("container-grant-failed")
	}
	got, err := windows.GetSecurityInfo(windows.Handle(f.Fd()), windows.SE_FILE_OBJECT, windows.DACL_SECURITY_INFORMATION)
	if err != nil || descriptorParts(got) != descriptorParts(sd) {
		return accessCode("container-grant-mismatch")
	}
	return nil
}

// runContainedProbe runs this binary's contained-probe command in the plan's
// container through StartContained and returns its outcomes.
func runContainedProbe(plan containerPlan, actions []probeAction, cwd string, extra map[string]string, seconds int) (map[string]probeOutcome, error) {
	self, err := os.Executable()
	if err != nil {
		return nil, err
	}
	spec, _ := json.Marshal(actions)
	native, err := newNativeScenario(512 * MiB)
	if err != nil {
		return nil, err
	}
	defer native.Close()
	logPath := filepath.Join(cwd, "..", "probe.log")
	log, err := os.OpenFile(logPath, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0600)
	if err != nil {
		return nil, err
	}
	defer log.Close()
	env := map[string]string{"SystemRoot": os.Getenv("SystemRoot"), "ZERUS_CONTAINED_PROBE": string(spec)}
	for k, v := range extra {
		env[k] = v
	}
	if err := native.StartContained(plan, self, []string{"contained-probe"}, env, cwd, log, func(processReceipt) error { return nil }); err != nil {
		return nil, err
	}
	code, timedOut, err := native.Wait(seconds, nil)
	if err != nil || timedOut || code != 0 {
		return nil, errors.New("contained probe did not finish cleanly")
	}
	data, err := os.ReadFile(logPath)
	if err != nil {
		return nil, err
	}
	return parseProbeOutcomes(data)
}
