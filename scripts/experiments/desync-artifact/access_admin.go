package main

// Person and device administration on the shared contract
// zerus-host-access-v1 (milestone 2, Phase 4). The executor service never
// writes the operator policy; these commands are run explicitly:
//
//   - by a person on their own device: access-request-device signs a device
//     enrollment for one host with the person's local key. The key is read
//     from the person's own file, never written anywhere else, never printed.
//   - by the host operator: access-admit-device verifies a request (contract,
//     the policy's host, the person key's self-proof, the signature, a
//     bounded unexpired window, no revocation, no duplicate) once (a ledger
//     of admitted request digests refuses replays) and writes the next policy
//     revision to a NEW file; access-revoke and access-rotate-device do the
//     same for revocation and device rotation; access-show prints the
//     verified host and the scope of every person and device.
//
// Membership is never implied by a device request: projects, roles and
// contexts come only from the operator's admission input.

import (
	"crypto/ed25519"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"time"
)

const (
	enrollmentRequestKind = "device-enrollment-request"
	maxEnrollmentLifetime = 400 * 24 * 3600 // seconds
	maxEnrollmentAge      = 7 * 24 * 3600   // a request must be admitted within a week of issue
)

// enrollmentRequest is what a device sends its host operator out of band.
type enrollmentRequest struct {
	Contract  string `json:"contract"`
	Kind      string `json:"kind"`
	Host      string `json:"host"`
	Person    string `json:"person"`
	PublicKey string `json:"public_key"`
	Label     string `json:"label"`
	Device    string `json:"device"`
	Issued    uint64 `json:"issued"`
	Expires   uint64 `json:"expires"`
	Signature string `json:"signature"`
}

type requestDeviceConfig struct {
	PersonSeedFile    string `json:"person_seed_file"`   // the person's local Ed25519 seed (base64url, 32 bytes)
	Host              string `json:"host"`               // the host fingerprint the person verified
	DeviceCertificate string `json:"device_certificate"` // this device's TLS client certificate (PEM)
	Label             string `json:"label"`
	LifetimeDays      int    `json:"lifetime_days"`
	Output            string `json:"output"`
}

type admitDeviceConfig struct {
	PolicyIn  string             `json:"policy_in"`
	PolicyOut string             `json:"policy_out"`
	Request   string             `json:"request"`
	Ledger    string             `json:"ledger"`
	Projects  []wireMember       `json:"projects,omitempty"` // refused: use grants
	Grants    []admissionProject `json:"grants,omitempty"`
	Context   *admissionContext  `json:"context,omitempty"`
	Revoke    []string           `json:"revoke_devices,omitempty"` // rotation: devices this admission replaces
}

type admissionProject struct {
	Project string `json:"project"`
	Role    string `json:"role"`
}

type admissionContext struct {
	Context string `json:"context"`
	Backend string `json:"backend"`
}

type revokeConfig struct {
	PolicyIn  string   `json:"policy_in"`
	PolicyOut string   `json:"policy_out"`
	Devices   []string `json:"devices,omitempty"`
	Persons   []string `json:"persons,omitempty"`
}

type showConfig struct {
	Policy string `json:"policy"`
}

func certificateFingerprint(path string) (string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return "", err
	}
	block, _ := pem.Decode(data)
	if block == nil || block.Type != "CERTIFICATE" {
		return "", errors.New("device certificate is not PEM")
	}
	if _, err := x509.ParseCertificate(block.Bytes); err != nil {
		return "", err
	}
	sum := sha256.Sum256(block.Bytes)
	return hex.EncodeToString(sum[:]), nil
}

// signEnrollment builds a request for one device and host.
func signEnrollment(seed []byte, host, device, label string, issued, expires uint64) (enrollmentRequest, error) {
	if len(seed) != ed25519.SeedSize || !accessID(host) || !accessID(device) || !accessLabel(label) || issued >= expires || expires-issued > maxEnrollmentLifetime {
		return enrollmentRequest{}, accessCode("request-invalid")
	}
	private := ed25519.NewKeyFromSeed(seed)
	public := private.Public().(ed25519.PublicKey)
	person := personIDOf(public)
	signature := ed25519.Sign(private, enrollmentStatement(host, person, device, issued, expires))
	return enrollmentRequest{Contract: hostAccessContract, Kind: enrollmentRequestKind, Host: host, Person: person,
		PublicKey: base64.RawURLEncoding.EncodeToString(public), Label: label, Device: device, Issued: issued, Expires: expires,
		Signature: base64.RawURLEncoding.EncodeToString(signature)}, nil
}

func runRequestDevice(configPath string) error {
	var cfg requestDeviceConfig
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	raw, err := os.ReadFile(cfg.PersonSeedFile)
	if err != nil {
		return errors.New("person key unavailable")
	}
	seed, err := base64.RawURLEncoding.DecodeString(string(trimSpace(raw)))
	if err != nil {
		return errors.New("person key is not a base64url seed")
	}
	device, err := certificateFingerprint(cfg.DeviceCertificate)
	if err != nil {
		return err
	}
	if cfg.LifetimeDays < 1 || cfg.LifetimeDays > 400 {
		return accessCode("request-invalid")
	}
	now := uint64(time.Now().Unix())
	request, err := signEnrollment(seed, cfg.Host, device, cfg.Label, now, now+uint64(cfg.LifetimeDays)*24*3600)
	if err != nil {
		return err
	}
	data, _ := json.MarshalIndent(request, "", "  ")
	if err := writeNewFile(cfg.Output, data); err != nil {
		return err
	}
	fmt.Printf("device enrollment request for host %s: person %s, device %s, expires %s\n", cfg.Host, request.Person, device,
		time.Unix(int64(request.Expires), 0).UTC().Format(time.RFC3339))
	return nil
}

func trimSpace(b []byte) []byte {
	for len(b) > 0 && (b[len(b)-1] == '\n' || b[len(b)-1] == '\r' || b[len(b)-1] == ' ') {
		b = b[:len(b)-1]
	}
	return b
}

// writeNewFile never replaces an existing file.
func writeNewFile(path string, data []byte) error {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return errors.New("output exists or is unavailable; choose a new file")
	}
	if _, err := f.Write(append(data, '\n')); err != nil {
		f.Close()
		return err
	}
	if err := f.Sync(); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

// checkEnrollmentRequest verifies a request against a policy at time now.
func checkEnrollmentRequest(r enrollmentRequest, wire wirePolicy, now uint64) (ed25519.PublicKey, error) {
	if r.Contract != hostAccessContract || r.Kind != enrollmentRequestKind {
		return nil, accessCode("request-contract")
	}
	if r.Host != wire.Host {
		return nil, accessCode("request-host-mismatch")
	}
	key, ok := personKey(r.PublicKey)
	if !ok || personIDOf(key) != r.Person || !accessID(r.Device) || !accessLabel(r.Label) {
		return nil, accessCode("request-identity")
	}
	if r.Issued >= r.Expires || r.Expires-r.Issued > maxEnrollmentLifetime || r.Expires > maxPolicyTime {
		return nil, accessCode("request-window")
	}
	if now+300 < r.Issued || now >= r.Expires || now > r.Issued+maxEnrollmentAge {
		return nil, accessCode("request-stale")
	}
	if !strictVerify(key, enrollmentStatement(r.Host, r.Person, r.Device, r.Issued, r.Expires), r.Signature) {
		return nil, accessCode("request-signature")
	}
	for _, revoked := range wire.RevokedDevices {
		if revoked == r.Device {
			return nil, accessCode("request-device-revoked")
		}
	}
	for _, revoked := range wire.RevokedPersons {
		if revoked == r.Person {
			return nil, accessCode("request-person-revoked")
		}
	}
	for _, d := range wire.Devices {
		if d.Device == r.Device {
			return nil, accessCode("request-duplicate-device")
		}
	}
	for _, p := range wire.Persons {
		if p.Person == r.Person && p.PublicKey != r.PublicKey {
			return nil, accessCode("request-key-mismatch")
		}
	}
	return key, nil
}

// readPolicyForAdmin reads and verifies a policy the operator administers,
// returning its wire form (host from the policy itself).
func readPolicyForAdmin(path string) (wirePolicy, error) {
	var wire wirePolicy
	data, err := os.ReadFile(path)
	if err != nil {
		return wire, err
	}
	if err := strictShape(data); err != nil {
		return wire, accessCode("parse")
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return wire, accessCode("parse")
	}
	if _, err := verifyAccessPolicy(data, wire.Host); err != nil {
		return wire, err
	}
	return wire, nil
}

// writeNextPolicy verifies the next revision and writes it to a new file.
func writeNextPolicy(wire wirePolicy, path string) error {
	wire.Revision++
	normalizePolicy(&wire)
	data, err := json.MarshalIndent(wire, "", "  ")
	if err != nil {
		return err
	}
	if _, err := verifyAccessPolicy(data, wire.Host); err != nil {
		return fmt.Errorf("the next policy would not verify: %w", err)
	}
	return writeNewFile(path, data)
}

// normalizePolicy keeps arrays non-null (strict shape refuses null).
func normalizePolicy(w *wirePolicy) {
	if w.Persons == nil {
		w.Persons = []wirePerson{}
	}
	if w.Devices == nil {
		w.Devices = []wireDevice{}
	}
	if w.Projects == nil {
		w.Projects = []wireProject{}
	}
	if w.Operators == nil {
		w.Operators = []wireOperator{}
	}
	if w.Contexts == nil {
		w.Contexts = []wireContext{}
	}
	if w.RevokedDevices == nil {
		w.RevokedDevices = []string{}
	}
	if w.RevokedPersons == nil {
		w.RevokedPersons = []string{}
	}
	for i := range w.Projects {
		if w.Projects[i].Members == nil {
			w.Projects[i].Members = []wireMember{}
		}
	}
}

// admitRequest applies a verified request to a policy.
func admitRequest(wire wirePolicy, r enrollmentRequest, grants []admissionProject, context *admissionContext, replaces []string) (wirePolicy, error) {
	known := false
	for _, p := range wire.Persons {
		if p.Person == r.Person {
			known = true
		}
	}
	if !known {
		wire.Persons = append(wire.Persons, wirePerson{r.Person, r.PublicKey, r.Label})
	}
	wire.Devices = append(wire.Devices, wireDevice{r.Device, r.Person, r.Issued, r.Expires, r.Signature})
	for _, g := range grants {
		found := false
		for i := range wire.Projects {
			if wire.Projects[i].Project != g.Project {
				continue
			}
			found = true
			for _, m := range wire.Projects[i].Members {
				if m.Person == r.Person {
					return wire, accessCode("admission-member-exists")
				}
			}
			wire.Projects[i].Members = append(wire.Projects[i].Members, wireMember{r.Person, g.Role})
		}
		if !found {
			wire.Projects = append(wire.Projects, wireProject{g.Project, []wireMember{{r.Person, g.Role}}})
		}
	}
	if context != nil {
		for _, c := range wire.Contexts {
			if c.Person == r.Person {
				return wire, accessCode("admission-context-exists")
			}
		}
		wire.Contexts = append(wire.Contexts, wireContext{r.Person, context.Context, context.Backend})
	}
	for _, device := range replaces {
		owned := false
		for _, d := range wire.Devices {
			if d.Device == device && d.Person == r.Person {
				owned = true
			}
		}
		if !owned {
			return wire, accessCode("rotation-not-owned")
		}
		wire.RevokedDevices = append(wire.RevokedDevices, device)
	}
	return wire, nil
}

type admissionLedger struct {
	Protocol int      `json:"protocol"`
	Host     string   `json:"host"`
	Admitted []string `json:"admitted"` // request digests, admitted once
}

func runAdmitDevice(configPath string) error {
	var cfg admitDeviceConfig
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	if len(cfg.Projects) != 0 {
		return errors.New("use grants for project roles")
	}
	wire, err := readPolicyForAdmin(cfg.PolicyIn)
	if err != nil {
		return err
	}
	raw, err := os.ReadFile(cfg.Request)
	if err != nil {
		return err
	}
	var request enrollmentRequest
	if err := strictControlBytes(raw, &request); err != nil {
		return accessCode("request-contract")
	}
	if _, err := checkEnrollmentRequest(request, wire, uint64(time.Now().Unix())); err != nil {
		return err
	}
	digest := sumBytes([]byte(request.Host + "\n" + request.Device + "\n" + request.Signature))
	var ledger admissionLedger
	if _, err := readControl(cfg.Ledger, &ledger); err != nil && !os.IsNotExist(err) {
		return errors.New("admission ledger needs review")
	} else if err == nil && ledger.Host != wire.Host {
		return errors.New("admission ledger belongs to another host")
	}
	for _, d := range ledger.Admitted {
		if d == digest {
			return accessCode("request-replayed")
		}
	}
	next, err := admitRequest(wire, request, cfg.Grants, cfg.Context, cfg.Revoke)
	if err != nil {
		return err
	}
	if err := writeNextPolicy(next, cfg.PolicyOut); err != nil {
		return err
	}
	ledger.Protocol, ledger.Host = deliveryProtocol, wire.Host
	ledger.Admitted = append(ledger.Admitted, digest)
	if err := durableJSON(cfg.Ledger, ledger); err != nil {
		return err
	}
	fmt.Printf("admitted device %s of person %s (%s) to host %s; install %s as revision %d\n", request.Device, request.Person, request.Label, wire.Host,
		filepath.Base(cfg.PolicyOut), wire.Revision+1)
	return nil
}

func runRevoke(configPath string) error {
	var cfg revokeConfig
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	wire, err := readPolicyForAdmin(cfg.PolicyIn)
	if err != nil {
		return err
	}
	if len(cfg.Devices)+len(cfg.Persons) == 0 {
		return errors.New("nothing to revoke")
	}
	for _, d := range cfg.Devices {
		if !accessID(d) {
			return accessCode("identifier")
		}
		wire.RevokedDevices = appendUnique(wire.RevokedDevices, d)
	}
	for _, p := range cfg.Persons {
		if !accessID(p) {
			return accessCode("identifier")
		}
		wire.RevokedPersons = appendUnique(wire.RevokedPersons, p)
	}
	return writeNextPolicy(wire, cfg.PolicyOut)
}

func appendUnique(list []string, value string) []string {
	for _, v := range list {
		if v == value {
			return list
		}
	}
	return append(list, value)
}

// policySummary is what access-show prints: the verified host and the scope
// of every person and device, without keys or signatures.
type policySummary struct {
	Host     string          `json:"host"`
	Revision uint64          `json:"revision"`
	Persons  []personSummary `json:"persons"`
}

type personSummary struct {
	Person   string          `json:"person"`
	Label    string          `json:"label"`
	Revoked  bool            `json:"revoked,omitempty"`
	Devices  []deviceSummary `json:"devices"`
	Projects []string        `json:"projects"`
	Operator []string        `json:"operator_rights,omitempty"`
	Context  string          `json:"context,omitempty"`
}

type deviceSummary struct {
	Device  string `json:"device"`
	Expires string `json:"expires"`
	Revoked bool   `json:"revoked,omitempty"`
}

func summarizePolicy(wire wirePolicy) policySummary {
	revokedDevice := map[string]bool{}
	for _, d := range wire.RevokedDevices {
		revokedDevice[d] = true
	}
	revokedPerson := map[string]bool{}
	for _, p := range wire.RevokedPersons {
		revokedPerson[p] = true
	}
	summary := policySummary{Host: wire.Host, Revision: wire.Revision, Persons: []personSummary{}}
	for _, p := range wire.Persons {
		s := personSummary{Person: p.Person, Label: p.Label, Revoked: revokedPerson[p.Person], Devices: []deviceSummary{}, Projects: []string{}}
		for _, d := range wire.Devices {
			if d.Person == p.Person {
				s.Devices = append(s.Devices, deviceSummary{d.Device, time.Unix(int64(d.Expires), 0).UTC().Format(time.RFC3339), revokedDevice[d.Device]})
			}
		}
		for _, project := range wire.Projects {
			for _, m := range project.Members {
				if m.Person == p.Person {
					s.Projects = append(s.Projects, project.Project+":"+m.Role)
				}
			}
		}
		sort.Strings(s.Projects)
		for _, o := range wire.Operators {
			if o.Person == p.Person {
				s.Operator = o.Rights
			}
		}
		for _, c := range wire.Contexts {
			if c.Person == p.Person {
				s.Context = c.Context + " (" + c.Backend + ")"
			}
		}
		summary.Persons = append(summary.Persons, s)
	}
	return summary
}

func runShow(configPath string) error {
	var cfg showConfig
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	wire, err := readPolicyForAdmin(cfg.Policy)
	if err != nil {
		return err
	}
	data, _ := json.MarshalIndent(summarizePolicy(wire), "", "  ")
	fmt.Println(string(data))
	return nil
}
