package main

// Multi-person executor access (epic zerus-un7n, milestone 1).
//
// With an "access" section the executor serves several people under one
// operator policy (host_access.go): any client certificate enrolled in the
// current policy may connect, and every request is authorized again, from a
// freshly checked policy, against the device that presented it. Each admitted
// job belongs to the person who submitted it; another person's job ID grants
// nothing. Arbitrary submitted code needs a qualified isolation backend, a real
// per-person security principal. This build registers none, so multi-person
// mode refuses to start and refuses before any job is written or admitted.

import (
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"errors"
	"io"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"golang.org/x/sys/windows"
)

// AccessConfig selects multi-person mode. Policy is the operator's private
// policy file, outside the executor root and every shared tree.
type AccessConfig struct {
	Policy string `json:"policy"`
	// Operator is the SID of the named host operator. It is pinned here, in
	// the executor's protected configuration, never in the policy it
	// authenticates. It must not be the executor's own account.
	Operator  string `json:"operator"`
	Isolation string `json:"isolation_backend"`
	// PersonJobs bounds one person's pending jobs (1..6, default 3); the
	// executor's global bound stays 6.
	PersonJobs int `json:"per_person_jobs,omitempty"`
}

// JobOwner binds an admitted job to the person who submitted it, through the
// device it came from, for one project.
type JobOwner struct {
	Person   string `json:"person"`
	Device   string `json:"device"`
	Project  string `json:"project"`
	Revision uint64 `json:"policy_revision"`
}

// An isolation backend runs a person's job under that person's own security
// principal (a separate non-administrator SID or UID, a sandbox or a VM).
type isolationBackend interface {
	// Qualified reports whether the backend is usable now; an error refuses.
	Qualified() error
}

// The backends this build provides. A production build registers none until a
// real per-person principal backend is implemented and qualified (milestone
// 2); tests register a synthetic one from a _test.go file, which no release
// build compiles. No configuration or environment value adds an entry.
var isolationBackends = map[string]isolationBackend{}

// Names that describe no security boundary. HOME or environment separation
// leaves one OS identity; a Windows Job Object limits a process tree's
// lifetime and resources but is not a security principal.
var notIsolation = map[string]bool{"": true, "none": true, "home": true, "env": true, "environment": true,
	"job-object": true, "jobobject": true, "windows-job-object": true, "same-user": true}

func requireIsolation(name string) error {
	if notIsolation[strings.ToLower(name)] {
		return errors.New("multi-person mode needs a real isolation backend; \"" + name + "\" is not a security boundary")
	}
	backend := isolationBackends[name]
	if backend == nil {
		return errors.New("isolation backend \"" + name + "\" is not available in this build; multi-person mode stays off")
	}
	return backend.Qualified()
}

// accessState holds the verified policy and the persisted high-water mark.
type accessState struct {
	mu     sync.Mutex
	policy *AccessPolicy
	now    func() time.Time
	// floor is the highest revision this process accepted, compared with the
	// durable high-water mark as well.
	floorRevision uint64
	floorDigest   string
	// trust verifies the owner and DACL of the policy and its folders. Without
	// it, every policy read is refused.
	trust policyTrust
	// modeSeen records that this process saw or wrote the mode record; its
	// later absence is a loss, not a fresh root.
	modeSeen bool
}

type accessHighWater struct {
	Protocol int    `json:"protocol"`
	Host     string `json:"host"`
	Revision uint64 `json:"revision"`
	Digest   string `json:"policy_sha256"`
}

type accessModeRecord struct {
	Protocol int    `json:"protocol"`
	Mode     string `json:"mode"`
	Host     string `json:"host"`
}

func (s *executorService) shared() bool { return s.cfg.Access != nil }

func (s *executorService) clock() time.Time {
	if s.access.now != nil {
		return s.access.now()
	}
	return time.Now()
}

// validateAccessConfig checks the access section before anything is written.
func validateAccessConfig(cfg ExecutorConfig) error {
	a := cfg.Access
	if cfg.Peer.Fingerprint != "" {
		return errors.New("multi-person mode replaces the single paired client; remove peer.fingerprint")
	}
	if cfg.Peer.Identity == "" {
		return errors.New("multi-person mode needs the executor's TLS identity")
	}
	if a.PersonJobs < 0 || a.PersonJobs > 6 {
		return errors.New("per_person_jobs must be 1..6")
	}
	if a.Operator == "" {
		return errors.New("multi-person mode needs the named operator's SID")
	}
	if !filepath.IsAbs(a.Policy) || filepath.Clean(a.Policy) != a.Policy {
		return errors.New("access policy needs an absolute, clean path")
	}
	root, err := filepath.Abs(cfg.Root)
	if err != nil {
		return err
	}
	if within(root, a.Policy) {
		return errors.New("access policy must live outside the executor root")
	}
	return requireIsolation(a.Isolation)
}

func within(root, path string) bool {
	rel, err := filepath.Rel(strings.ToLower(filepath.Clean(root)), strings.ToLower(filepath.Clean(path)))
	return err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) && !filepath.IsAbs(rel)
}

// policyReadHook runs between the trust check and the read (tests only).
var policyReadHook func()

// readPolicyFile reads the policy through one handle opened without
// following a link or reparse point (nativeOpen adds
// FILE_FLAG_OPEN_REPARSE_POINT), while handles on every ancestor, opened the
// same way and without delete sharing, keep them from being renamed or
// replaced. A reparse point, a directory, a second hard link or a final path
// other than the configured one is refused. trust then checks the volume and
// the owner and DACL of every held folder and of the file, and the bytes are
// read through the same file handle it checked.
func readPolicyFile(path string, trust policyTrust) ([]byte, error) {
	if trust == nil {
		return nil, errNoPolicyTrust
	}
	parents, err := holdPolicyChain(filepath.Dir(path))
	defer closeFiles(parents)
	if err != nil {
		return nil, errors.New("access policy folder is not a plain local folder")
	}
	f, err := nativeOpen(path, windows.GENERIC_READ, windows.FILE_SHARE_READ, windows.OPEN_EXISTING, 0)
	if err != nil {
		return nil, errors.New("access policy unavailable")
	}
	defer f.Close()
	var info windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(windows.Handle(f.Fd()), &info); err != nil {
		return nil, err
	}
	if info.FileAttributes&(windows.FILE_ATTRIBUTE_REPARSE_POINT|windows.FILE_ATTRIBUTE_DIRECTORY) != 0 {
		return nil, errors.New("access policy is not a regular file")
	}
	if info.NumberOfLinks != 1 {
		return nil, errors.New("access policy has another hard link")
	}
	if uint64(info.FileSizeHigh)<<32|uint64(info.FileSizeLow) > maxPolicyBytes {
		return nil, accessCode("too-large")
	}
	final, err := finalPath(f)
	if err != nil || !strings.EqualFold(final, filepath.Clean(path)) {
		return nil, errors.New("access policy resolves to another path")
	}
	if err := trust.verify(parents, f); err != nil {
		return nil, err
	}
	if policyReadHook != nil {
		policyReadHook()
	}
	data, err := io.ReadAll(io.LimitReader(f, maxPolicyBytes+1))
	if err != nil {
		return nil, err
	}
	if len(data) > maxPolicyBytes {
		return nil, accessCode("too-large")
	}
	return data, nil
}

func finalPath(f *os.File) (string, error) {
	buffer := make([]uint16, 1024)
	n, err := windows.GetFinalPathNameByHandle(windows.Handle(f.Fd()), &buffer[0], uint32(len(buffer)), 0)
	if err != nil || int(n) >= len(buffer) {
		return "", errors.New("final path unavailable")
	}
	return strings.TrimPrefix(windows.UTF16ToString(buffer[:n]), `\\?\`), nil
}

// currentPolicy verifies the policy file now. A missing, unreadable, invalid,
// replaced or rolled-back policy fails closed; there is no fallback to an
// earlier grant or to the single-pin mode. The highest accepted revision is
// durable in the executor root, so a rollback is refused after a restart too.
func (s *executorService) currentPolicy() (*AccessPolicy, error) {
	s.access.mu.Lock()
	defer s.access.mu.Unlock()
	data, err := readPolicyFile(s.cfg.Access.Policy, s.access.trust)
	if err != nil {
		return nil, err
	}
	digest := sumBytes(data)
	// The durable mark, the mode record and the in-memory floor are checked on
	// every call, before any cached verification is reused: a cache hit only
	// skips parsing and signature checks of bytes already verified.
	recorded, err := s.checkAccessMode()
	if err != nil {
		return nil, err
	}
	path := filepath.Join(s.cfg.Root, "access-high-water.json")
	var mark accessHighWater
	if _, err := readControl(path, &mark); err == nil {
		if mark.Protocol != deliveryProtocol || mark.Host != s.identity {
			return nil, errors.New("access high-water record changed")
		}
		if mark.Revision == 0 || len(mark.Digest) != 64 {
			return nil, accessCode("high-water-unreadable")
		}
		// The mark never moves below or away from what this process accepted.
		if mark.Revision < s.access.floorRevision {
			return nil, accessCode("high-water-rollback")
		}
		if mark.Revision == s.access.floorRevision && mark.Digest != s.access.floorDigest {
			return nil, accessCode("high-water-replaced")
		}
	} else if !os.IsNotExist(err) {
		return nil, accessCode("high-water-unreadable")
	} else if recorded || s.access.floorRevision != 0 {
		// Only a root that never served several people may start a mark: the
		// mark is written before the mode record, so a missing mark beside a
		// mode record, or after this process accepted a revision, is a loss.
		return nil, accessCode("high-water-lost")
	}
	var policy *AccessPolicy
	if s.access.policy != nil && s.access.policy.Digest == digest {
		policy = s.access.policy
	} else if policy, err = verifyAccessPolicy(data, s.identity); err != nil {
		return nil, err
	}
	for _, floor := range []struct {
		revision uint64
		digest   string
	}{{mark.Revision, mark.Digest}, {s.access.floorRevision, s.access.floorDigest}} {
		if policy.Revision < floor.revision {
			return nil, accessCode("policy-rollback")
		}
		if policy.Revision == floor.revision && policy.Digest != floor.digest {
			return nil, accessCode("policy-replaced")
		}
	}
	if policy.Revision > mark.Revision {
		if err := durableJSON(path, accessHighWater{deliveryProtocol, s.identity, policy.Revision, policy.Digest}); err != nil {
			return nil, err
		}
	}
	s.access.policy = policy
	s.access.floorRevision, s.access.floorDigest = policy.Revision, policy.Digest
	return policy, nil
}

// checkAccessMode reads the mode record under the access lock. A record that
// names another mode or host, or that disappears after this process saw it,
// refuses; an unreadable record is never a fresh root.
func (s *executorService) checkAccessMode() (bool, error) {
	var mode accessModeRecord
	if _, err := readControl(filepath.Join(s.cfg.Root, "access-mode.json"), &mode); err == nil {
		if mode.Protocol != deliveryProtocol || mode.Mode != "multi-person" || mode.Host != s.identity {
			return true, accessCode("access-mode-changed")
		}
		s.access.modeSeen = true
		return true, nil
	} else if !os.IsNotExist(err) {
		return true, accessCode("access-mode-unreadable")
	}
	if s.access.modeSeen {
		return true, accessCode("access-mode-lost")
	}
	return false, nil
}

// executionContainers holds, per job being run by this executor, the check
// that a scenario's container is exactly the one the current policy gives
// that job's owner, context and project.
var executionContainers sync.Map

// verifyLaunchEnvelope is called by the scenario before it applies grants or
// launches; a shared executor without a registered verifier refuses.
func verifyLaunchEnvelope(job string, spec *containerSpec) error {
	check, ok := executionContainers.Load(job)
	if !ok {
		if sharedExecution.Load() {
			return accessCode("container-envelope-missing")
		}
		return nil
	}
	return check.(func(*containerSpec) error)(spec)
}

// sameContainer compares two specifications exactly.
func sameContainer(a, b *containerSpec) bool {
	if a == nil || b == nil || a.Name != b.Name || a.SID != b.SID || len(a.Capabilities) != len(b.Capabilities) || len(a.CapabilityNames) != len(b.CapabilityNames) {
		return false
	}
	for i := range a.Capabilities {
		if a.Capabilities[i] != b.Capabilities[i] || a.CapabilityNames[i] != b.CapabilityNames[i] {
			return false
		}
	}
	return true
}

// containerBackend is an isolation backend that runs jobs in per-person
// containers.
type containerBackend interface {
	QualifiedFor(root string, plan containerPlan, origin containerOrigin, trust string) error
}

// containerFor plans the job's container from the current policy and runs
// the backend's runtime probe. Single-person mode and backends without
// containers (the synthetic test backend) return no container; a shared
// executor's scenario then refuses to launch.
func (s *executorService) containerFor(value ExecutorSubmission) (*containerSpec, error) {
	if !s.shared() {
		return nil, nil
	}
	backend, ok := isolationBackends[s.cfg.Access.Isolation].(containerBackend)
	if !ok {
		return nil, nil
	}
	if value.Owner == nil {
		return nil, accessCode("owner-missing")
	}
	policy, err := s.currentPolicy()
	if err != nil {
		return nil, accessCode("policy-unavailable")
	}
	installation := sumBytes([]byte(s.root))
	plan, err := planContainer(installation, s.identity, *value.Owner, policy, networkGrant{})
	if err != nil {
		return nil, err
	}
	origin := containerOrigin{Installation: installation, Host: s.identity, Person: value.Owner.Person, Context: policy.Contexts[value.Owner.Person].Context}
	if err := backend.QualifiedFor(s.cfg.Root, plan, origin, s.cfg.Access.Operator+"\n"+policy.Digest); err != nil {
		return nil, err
	}
	return specOf(plan), nil
}

// expectedContainer recomputes the job's container from the CURRENT policy
// after re-authorizing its owner, device, membership and context.
func (s *executorService) expectedContainer(value ExecutorSubmission) (*containerSpec, error) {
	if err := s.authorizeExecution(value); err != nil {
		return nil, err
	}
	policy, err := s.currentPolicy()
	if err != nil {
		return nil, accessCode("policy-unavailable")
	}
	plan, err := planContainer(sumBytes([]byte(s.root)), s.identity, *value.Owner, policy, networkGrant{})
	if err != nil {
		return nil, err
	}
	return specOf(plan), nil
}

// accessModeRecorded reports whether the root has served several people. An
// unreadable record is an error, never a fresh root.
func accessModeRecorded(root string) (bool, error) {
	var mode accessModeRecord
	if _, err := readControl(filepath.Join(root, "access-mode.json"), &mode); err == nil {
		return true, nil
	} else if os.IsNotExist(err) {
		return false, nil
	} else {
		return false, err
	}
}

// prepareAccessRoot runs at start in multi-person mode, after the policy and
// isolation checks: it records the mode and refuses ownerless receipts.
func (s *executorService) prepareAccessRoot() error {
	path := filepath.Join(s.cfg.Root, "access-mode.json")
	var mode accessModeRecord
	if _, err := readControl(path, &mode); err == nil {
		if mode.Protocol != deliveryProtocol || mode.Mode != "multi-person" || mode.Host != s.identity {
			return errors.New("access mode record changed")
		}
	} else if os.IsNotExist(err) {
		if len(s.queue.Jobs) != 0 {
			return errors.New("collect or release the single-person jobs before enabling multi-person mode")
		}
		if err := durableJSON(path, accessModeRecord{deliveryProtocol, "multi-person", s.identity}); err != nil {
			return err
		}
	} else {
		return err
	}
	s.access.mu.Lock()
	s.access.modeSeen = true
	s.access.mu.Unlock()
	for _, job := range s.queue.Jobs {
		value, err := s.readSubmission(job)
		if err != nil {
			return err
		}
		if value.Owner == nil {
			return errors.New("an ownerless receipt cannot be adopted in multi-person mode")
		}
	}
	return nil
}

// refuseLegacyAfterShared keeps a root that once served several people from
// silently returning to the single-pin mode.
func refuseLegacyAfterShared(root string) error {
	var mode accessModeRecord
	if _, err := readControl(filepath.Join(root, "access-mode.json"), &mode); err == nil || !os.IsNotExist(err) {
		return errors.New("this executor root is in multi-person mode; its access section is required")
	}
	return nil
}

func deviceOf(state *tls.ConnectionState) string {
	if state == nil || len(state.PeerCertificates) != 1 {
		return ""
	}
	sum := sha256.Sum256(state.PeerCertificates[0].Raw)
	return hex.EncodeToString(sum[:])
}

// verifyEnrolledDevice admits a TLS client whose self-signed client
// certificate is enrolled in the current policy. The request handler checks
// authorization again for each request.
func (s *executorService) verifyEnrolledDevice(state tls.ConnectionState) error {
	if len(state.PeerCertificates) != 1 {
		return errors.New("device identity required")
	}
	leaf := state.PeerCertificates[0]
	roots := x509.NewCertPool()
	roots.AddCert(leaf)
	if _, err := leaf.Verify(x509.VerifyOptions{Roots: roots, KeyUsages: []x509.ExtKeyUsage{x509.ExtKeyUsageClientAuth}, CurrentTime: time.Now()}); err != nil {
		return err
	}
	policy, err := s.currentPolicy()
	if err != nil {
		return errors.New("access policy unavailable")
	}
	if _, err := authorizeAccess(policy, deviceOf(&state), uint64(s.clock().Unix()), "capabilities", ""); err != nil {
		return err
	}
	return nil
}

func (s *executorService) accessListener() (net.Listener, string, string, error) {
	cfg := s.cfg.Peer
	if _, err := readIdleBound(cfg.ReadIdle); err != nil {
		return nil, "", "", err
	}
	address := cfg.Listen
	if address == "" {
		address = "127.0.0.1:0"
	}
	if err := privateAddress(address, true); err != nil {
		return nil, "", "", err
	}
	pair, pin, err := loadIdentity(cfg.Identity)
	if err != nil {
		return nil, "", "", err
	}
	listener, err := net.Listen("tcp4", address)
	if err != nil {
		return nil, "", "", err
	}
	config := &tls.Config{MinVersion: tls.VersionTLS13, Certificates: []tls.Certificate{pair}, ClientAuth: tls.RequireAnyClientCert,
		VerifyConnection: s.verifyEnrolledDevice, SessionTicketsDisabled: true}
	return tls.NewListener(listener, config), "https://" + listener.Addr().String(), pin, nil
}

// accessCaller is an authenticated request's person and device under the
// policy that authorized it.
type accessCaller struct {
	Person, Device string
	Policy         *AccessPolicy
}

func accessRefused(w http.ResponseWriter, code string) {
	http.Error(w, "access refused: "+code, http.StatusForbidden)
}

// authenticate resolves the caller for operation; project applies to project
// operations. It writes the refusal itself.
func (s *executorService) authenticate(w http.ResponseWriter, r *http.Request, operation, project string) (*accessCaller, accessDecision, bool) {
	policy, err := s.currentPolicy()
	if err != nil {
		http.Error(w, "access policy unavailable", http.StatusServiceUnavailable)
		return nil, accessDecision{}, false
	}
	device := deviceOf(r.TLS)
	decision, err := authorizeAccess(policy, device, uint64(s.clock().Unix()), operation, project)
	if err != nil {
		accessRefused(w, err.Error())
		return nil, accessDecision{}, false
	}
	return &accessCaller{decision.Person, device, policy}, decision, true
}

// mutationBound checks a mutating request's binding to the current policy
// revision and its deadline. A stale revision is refused, so the client
// re-reads capabilities; earlier receipts stay valid regardless.
func (s *executorService) mutationBound(w http.ResponseWriter, r *http.Request, caller *accessCaller) bool {
	revision, err := strconv.ParseUint(r.Header.Get("Zerus-Policy-Revision"), 10, 64)
	if err != nil || revision != caller.Policy.Revision {
		http.Error(w, "policy revision changed; read capabilities again", http.StatusConflict)
		return false
	}
	deadline, err := strconv.ParseInt(r.Header.Get("Zerus-Request-Deadline"), 10, 64)
	now := s.clock().UnixMilli()
	if err != nil || deadline <= now || deadline > now+5*60*1000 {
		http.Error(w, "request deadline outside its window", http.StatusConflict)
		return false
	}
	return true
}

// executionContext checks that the person has a context on the configured,
// qualified isolation backend.
func (s *executorService) executionContext(policy *AccessPolicy, person string) error {
	context, ok := policy.Contexts[person]
	if !ok {
		return accessCode("context-missing")
	}
	if context.Backend != s.cfg.Access.Isolation {
		return accessCode("context-backend")
	}
	if requireIsolation(context.Backend) != nil {
		return accessCode("isolation-unavailable")
	}
	return nil
}

// authorizeExecution repeats admission's checks immediately before a queued
// job deploys or runs: a revoked device, person, membership or context, or an
// unavailable backend, keeps it from launching.
func (s *executorService) authorizeExecution(value ExecutorSubmission) error {
	if value.Owner == nil {
		return accessCode("owner-missing")
	}
	policy, err := s.currentPolicy()
	if err != nil {
		return accessCode("policy-unavailable")
	}
	decision, err := authorizeAccess(policy, value.Owner.Device, uint64(s.clock().Unix()), "jobs.submit", value.Owner.Project)
	if err != nil {
		return err
	}
	if decision.Person != value.Owner.Person {
		return accessCode("owner-changed")
	}
	return s.executionContext(policy, value.Owner.Person)
}

// fairOrder interleaves runnable jobs by owner, round robin, starting after
// the person served last; each person's jobs keep their queue order.
func fairOrder(values []ExecutorSubmission, last string) []ExecutorSubmission {
	var persons []string
	byPerson := map[string][]ExecutorSubmission{}
	for _, value := range values {
		person := ""
		if value.Owner != nil {
			person = value.Owner.Person
		}
		if byPerson[person] == nil {
			persons = append(persons, person)
		}
		byPerson[person] = append(byPerson[person], value)
	}
	start := 0
	for i, person := range persons {
		if person == last {
			start = i + 1
		}
	}
	var order []ExecutorSubmission
	for len(order) < len(values) {
		for i := range persons {
			person := persons[(start+i)%len(persons)]
			if jobs := byPerson[person]; len(jobs) > 0 {
				order = append(order, jobs[0])
				byPerson[person] = jobs[1:]
			}
		}
	}
	return order
}

func (s *executorService) personJobLimit() int {
	if s.cfg.Access.PersonJobs == 0 {
		return 3
	}
	return s.cfg.Access.PersonJobs
}

func sameOwner(a, b *JobOwner) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return a.Person == b.Person && a.Project == b.Project
}

// ownedJobs counts one person's pending jobs; the caller holds s.mu.
func (s *executorService) ownedJobs(person string) int {
	count := 0
	for _, job := range s.queue.Jobs {
		if value, err := s.readSubmission(job); err == nil && value.Owner != nil && value.Owner.Person == person {
			count++
		}
	}
	return count
}

// runNextShared runs the next runnable job in fair order, after checking its
// owner's current authorization. It reports whether it handled a job.
func (s *executorService) runNextShared(jobs []string) bool {
	var runnable []ExecutorSubmission
	for _, job := range jobs {
		if value, err := s.readSubmission(job); err == nil && (value.Phase == "queued" || value.Phase == "deploying") {
			runnable = append(runnable, value)
		}
	}
	if len(runnable) == 0 {
		return false
	}
	value := fairOrder(runnable, s.lastPerson)[0]
	if value.Owner != nil {
		s.lastPerson = value.Owner.Person
	}
	if err := s.authorizeExecution(value); err != nil {
		// Nothing was deployed or started: the owner, or an operator, may
		// release this receipt.
		_ = s.phase(&value, "access_refused", "access: "+err.Error())
		return true
	}
	s.runOne(value)
	return true
}

// authorizeJob checks a per-job request in multi-person mode. Only the owner
// sees or controls a job; to anyone else it does not exist. An operator's
// resource-cancel right reaches another person's job only to cancel it or to
// release a receipt that access refused before it ever ran.
func (s *executorService) authorizeJob(w http.ResponseWriter, r *http.Request, value ExecutorSubmission, parts []string) bool {
	caller, _, ok := s.authenticate(w, r, "jobs.list", "")
	if !ok {
		return false
	}
	route := ""
	if len(parts) > 1 {
		route = parts[1]
	}
	mutation := r.Method != http.MethodGet
	cancel := route == "cancel" && r.Method == http.MethodPost && len(parts) == 2
	release := route == "release" && r.Method == http.MethodPost && len(parts) == 2
	owner := value.Owner != nil && value.Owner.Person == caller.Person
	if value.Owner == nil {
		http.Error(w, "unknown job", http.StatusNotFound)
		return false
	}
	operation := "jobs.read"
	switch {
	case cancel:
		operation = "jobs.cancel"
	case mutation:
		operation = "jobs.control"
	}
	if !owner {
		operatorReach := cancel || (release && value.Phase == "access_refused")
		if !operatorReach {
			http.Error(w, "unknown job", http.StatusNotFound)
			return false
		}
		operation = "jobs.cancel"
	}
	caller, decision, ok := s.authenticate(w, r, operation, value.Owner.Project)
	if !ok {
		return false
	}
	if !owner && decision.Via != "operator" {
		http.Error(w, "unknown job", http.StatusNotFound)
		return false
	}
	if mutation && !s.mutationBound(w, r, caller) {
		return false
	}
	if cancel {
		via := "owner"
		if !owner {
			via = "operator"
		}
		s.cancelJob(w, r, value, caller, via)
		return false
	}
	return true
}

// slotRelative is a delivery slot's folder under the executor root. In
// multi-person mode slots belong to the job's project: members of one project
// share one managed copy, and another project's slot of the same name is a
// different folder. The worker runs one job at a time, so a delivery never
// replaces a tree that another job is executing.
func slotRelative(value ExecutorSubmission, kind string) string {
	if value.Owner == nil {
		return kind + "/" + value.Request.Slot
	}
	return "project-" + kind + "/" + value.Owner.Project + "/" + value.Request.Slot
}

// listable is a list row the caller may see now: their own job, in a project
// they may still read under the current policy.
func (s *executorService) listable(caller *accessCaller, value ExecutorSubmission) bool {
	if value.Owner == nil || value.Owner.Person != caller.Person {
		return false
	}
	_, err := authorizeAccess(caller.Policy, caller.Device, uint64(s.clock().Unix()), "jobs.read", value.Owner.Project)
	return err == nil
}

// The execution admission boundary. runOne registers a check for the job it is
// running; runScenario calls it immediately before native process creation
// (and again inside the creation callback, before CreateProcess), so a
// revocation during delivery or the pre-launch quiet wait still prevents the
// launch. It is process memory in trusted code, never a configuration field.
var (
	executionAdmissions sync.Map // job ID -> func() error
	sharedExecution     atomic.Bool
)

func admitExecution(job string) error {
	if check, ok := executionAdmissions.Load(job); ok {
		return check.(func() error)()
	}
	if sharedExecution.Load() {
		return accessCode("admission-missing")
	}
	return nil
}

// refusedDuringDeploy repeats the execution check between deployment steps.
func (s *executorService) refusedDuringDeploy(value *ExecutorSubmission) bool {
	if !s.shared() {
		return false
	}
	if cancelRequested(value.Request.Job) {
		_ = s.phase(value, "cancelled", "cancelled before launch")
		return true
	}
	if err := s.authorizeExecution(*value); err != nil {
		_ = s.phase(value, "access_refused", "access: "+err.Error())
		return true
	}
	return false
}
