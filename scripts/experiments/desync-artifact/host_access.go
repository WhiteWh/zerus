package main

// Host access contract zerus-host-access-v1 (epic zerus-un7n), the executor's
// implementation of contracts/host-access-v1. src/host_access (full Zerus)
// implements the same contract; both must agree on every shared fixture.

import (
	"bytes"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strconv"
	"unicode"
	"unicode/utf8"

	"filippo.io/edwards25519"
)

const (
	hostAccessContract = "zerus-host-access-v1"
	maxPolicyBytes     = 1024 * 1024
	maxPolicyTime      = uint64(1)<<53 - 1
)

// accessCode is a contract failure or refusal code.
type accessCode string

func (c accessCode) Error() string { return string(c) }

type wirePolicy struct {
	Contract       string         `json:"contract"`
	Host           string         `json:"host"`
	Revision       uint64         `json:"revision"`
	Persons        []wirePerson   `json:"persons"`
	Devices        []wireDevice   `json:"devices"`
	Projects       []wireProject  `json:"projects"`
	Operators      []wireOperator `json:"operators"`
	Contexts       []wireContext  `json:"contexts"`
	RevokedDevices []string       `json:"revoked_devices"`
	RevokedPersons []string       `json:"revoked_persons"`
}
type wirePerson struct {
	Person    string `json:"person"`
	PublicKey string `json:"public_key"`
	Label     string `json:"label"`
}
type wireDevice struct {
	Device    string `json:"device"`
	Person    string `json:"person"`
	Issued    uint64 `json:"issued"`
	Expires   uint64 `json:"expires"`
	Signature string `json:"signature"`
}
type wireProject struct {
	Project string       `json:"project"`
	Members []wireMember `json:"members"`
}
type wireMember struct {
	Person string `json:"person"`
	Role   string `json:"role"`
}
type wireOperator struct {
	Person string   `json:"person"`
	Rights []string `json:"rights"`
}
type wireContext struct {
	Person  string `json:"person"`
	Context string `json:"context"`
	Backend string `json:"backend"`
}

// The members each object kind accepts, and the kind of their array elements.
var policyShape = map[string]map[string]string{
	"policy": {"contract": "", "host": "", "revision": "", "persons": "person", "devices": "device",
		"projects": "project", "operators": "operator", "contexts": "context", "revoked_devices": "", "revoked_persons": ""},
	"person":   {"person": "", "public_key": "", "label": ""},
	"device":   {"device": "", "person": "", "issued": "", "expires": "", "signature": ""},
	"project":  {"project": "", "members": "member"},
	"member":   {"person": "", "role": ""},
	"operator": {"person": "", "rights": ""},
	"context":  {"person": "", "context": "", "backend": ""},
}

// strictShape refuses what encoding/json would otherwise accept: duplicate or
// case-variant member names, unknown members, null, and anything after the
// document. Mistyped values are left to the decoder, which refuses them too.
func strictShape(data []byte) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := walkShape(decoder, "policy", true); err != nil {
		return err
	}
	if _, err := decoder.Token(); err != io.EOF {
		return errors.New("trailing data")
	}
	return nil
}

func walkShape(decoder *json.Decoder, kind string, requireObject bool) error {
	token, err := decoder.Token()
	if err != nil {
		return err
	}
	switch value := token.(type) {
	case nil:
		return errors.New("null")
	case json.Delim:
		switch value {
		case '{':
			members, known := policyShape[kind]
			if !known {
				return errors.New("unexpected object")
			}
			seen := map[string]bool{}
			for decoder.More() {
				nameToken, err := decoder.Token()
				if err != nil {
					return err
				}
				name := nameToken.(string)
				child, allowed := members[name]
				if !allowed || seen[name] {
					return errors.New("unknown or repeated member")
				}
				seen[name] = true
				if err := walkShape(decoder, child, false); err != nil {
					return err
				}
			}
			_, err := decoder.Token()
			return err
		case '[':
			if requireObject {
				return errors.New("not an object")
			}
			for decoder.More() {
				if err := walkShape(decoder, kind, false); err != nil {
					return err
				}
			}
			_, err := decoder.Token()
			return err
		}
		return errors.New("unexpected delimiter")
	default:
		if requireObject {
			return errors.New("not an object")
		}
		return nil
	}
}

type accessRole int

const (
	roleNone accessRole = iota
	roleReader
	roleContributor
)

func (r accessRole) name() string {
	switch r {
	case roleReader:
		return "reader"
	case roleContributor:
		return "contributor"
	}
	return ""
}

type accessEnrollment struct {
	Person          string
	Issued, Expires uint64
}
type accessContext struct{ Context, Backend string }

// AccessPolicy is a verified policy for one host.
type AccessPolicy struct {
	Host            string
	Revision        uint64
	Digest          string
	Devices         map[string]accessEnrollment
	Projects        map[string]map[string]accessRole
	CancelOperators map[string]bool
	Contexts        map[string]accessContext
	Persons         map[string]bool
	RevokedDevices  map[string]bool
	RevokedPersons  map[string]bool
}

func accessID(value string) bool {
	if len(value) != 64 {
		return false
	}
	for _, c := range []byte(value) {
		if !(c >= '0' && c <= '9' || c >= 'a' && c <= 'f') {
			return false
		}
	}
	return true
}

func accessName(value string) bool {
	if value == "" || len(value) > 64 {
		return false
	}
	for i, c := range []byte(value) {
		lower := c >= 'a' && c <= 'z' || c >= '0' && c <= '9'
		if !lower && (i == 0 || (c != '.' && c != '_' && c != '-')) {
			return false
		}
	}
	return true
}

func accessLabel(value string) bool {
	if !utf8.ValidString(value) {
		return false
	}
	count := utf8.RuneCountInString(value)
	if count < 1 || count > 160 {
		return false
	}
	for _, r := range value {
		if unicode.IsControl(r) {
			return false
		}
	}
	return true
}

func decodeFixed(value string, size int) ([]byte, bool) {
	raw, err := base64.RawURLEncoding.DecodeString(value)
	if err != nil || len(raw) != size || base64.RawURLEncoding.EncodeToString(raw) != value {
		return nil, false
	}
	return raw, true
}

// canonicalPoint reports whether a 32-byte point encoding has y < p. The
// decoder below accepts non-canonical encodings, which the contract refuses.
func canonicalPoint(raw []byte) bool {
	// p = 2^255 - 19, little-endian: ed ff .. ff 7f.
	y := append([]byte{}, raw...)
	y[31] &= 0x7f
	for i := 31; i >= 0; i-- {
		limit := byte(0xff)
		if i == 31 {
			limit = 0x7f
		} else if i == 0 {
			limit = 0xed
		}
		if y[i] != limit {
			return y[i] < limit
		}
	}
	return false
}

// strictPoint decodes a canonical point and refuses one of small order (its
// cofactor multiple is the identity), as ed25519-dalek's is_weak does. The
// decoding is filippo.io/edwards25519, the standalone form of the Go
// standard library's own implementation.
func strictPoint(raw []byte) bool {
	if len(raw) != 32 || !canonicalPoint(raw) {
		return false
	}
	point, err := new(edwards25519.Point).SetBytes(raw)
	if err != nil {
		return false
	}
	return new(edwards25519.Point).MultByCofactor(point).Equal(edwards25519.NewIdentityPoint()) != 1
}

func personKey(value string) (ed25519.PublicKey, bool) {
	raw, ok := decodeFixed(value, ed25519.PublicKeySize)
	if !ok || !strictPoint(raw) {
		return nil, false
	}
	return ed25519.PublicKey(raw), true
}

func personIDOf(key ed25519.PublicKey) string {
	sum := sha256.Sum256(key)
	return hex.EncodeToString(sum[:])
}

func enrollmentStatement(host, person, device string, issued, expires uint64) []byte {
	return []byte(hostAccessContract + "/device-enrollment\nhost=" + host + "\nperson=" + person + "\ndevice=" + device +
		"\nissued=" + strconv.FormatUint(issued, 10) + "\nexpires=" + strconv.FormatUint(expires, 10) + "\n")
}

func strictVerify(key ed25519.PublicKey, message []byte, signature string) bool {
	raw, ok := decodeFixed(signature, ed25519.SignatureSize)
	if !ok || !strictPoint(raw[:32]) {
		return false
	}
	// crypto/ed25519 refuses a non-canonical S (s >= l).
	return ed25519.Verify(key, message, raw)
}

// verifyAccessPolicy verifies a policy document for expectedHost, in the
// order contracts/host-access-v1/README.md fixes.
func verifyAccessPolicy(data []byte, expectedHost string) (*AccessPolicy, error) {
	if len(data) > maxPolicyBytes {
		return nil, accessCode("too-large")
	}
	var wire wirePolicy
	if strictShape(data) != nil {
		return nil, accessCode("malformed")
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if decoder.Decode(&wire) != nil {
		return nil, accessCode("malformed")
	}
	if wire.Contract != hostAccessContract {
		return nil, accessCode("contract")
	}
	if !accessID(wire.Host) {
		return nil, accessCode("host")
	}
	if wire.Host != expectedHost {
		return nil, accessCode("host-mismatch")
	}
	if wire.Revision < 1 || wire.Revision > maxPolicyTime {
		return nil, accessCode("revision")
	}
	if len(wire.Persons) > 256 || len(wire.Devices) > 1024 || len(wire.Projects) > 256 || len(wire.Operators) > 64 ||
		len(wire.Contexts) > 256 || len(wire.RevokedDevices) > 4096 || len(wire.RevokedPersons) > 256 {
		return nil, accessCode("limits")
	}
	for _, project := range wire.Projects {
		if len(project.Members) > 256 {
			return nil, accessCode("limits")
		}
	}
	for _, operator := range wire.Operators {
		if len(operator.Rights) > 4 {
			return nil, accessCode("limits")
		}
	}
	sum := sha256.Sum256(data)
	policy := &AccessPolicy{Host: wire.Host, Revision: wire.Revision, Digest: hex.EncodeToString(sum[:]),
		Devices: map[string]accessEnrollment{}, Projects: map[string]map[string]accessRole{}, CancelOperators: map[string]bool{},
		Contexts: map[string]accessContext{}, Persons: map[string]bool{}, RevokedDevices: map[string]bool{}, RevokedPersons: map[string]bool{}}
	keys := map[string]ed25519.PublicKey{}
	for _, person := range wire.Persons {
		if !accessID(person.Person) || !accessLabel(person.Label) {
			return nil, accessCode("identifier")
		}
		key, ok := personKey(person.PublicKey)
		if !ok {
			return nil, accessCode("person-key")
		}
		if personIDOf(key) != person.Person {
			return nil, accessCode("person-id")
		}
		if keys[person.Person] != nil {
			return nil, accessCode("duplicate")
		}
		keys[person.Person] = key
		policy.Persons[person.Person] = true
	}
	for _, device := range wire.Devices {
		if !accessID(device.Device) || !accessID(device.Person) {
			return nil, accessCode("identifier")
		}
		if _, seen := policy.Devices[device.Device]; seen {
			return nil, accessCode("duplicate")
		}
		key := keys[device.Person]
		if key == nil {
			return nil, accessCode("unknown-person")
		}
		if device.Issued >= device.Expires || device.Expires > maxPolicyTime {
			return nil, accessCode("window")
		}
		if !strictVerify(key, enrollmentStatement(wire.Host, device.Person, device.Device, device.Issued, device.Expires), device.Signature) {
			return nil, accessCode("signature")
		}
		policy.Devices[device.Device] = accessEnrollment{device.Person, device.Issued, device.Expires}
	}
	for _, project := range wire.Projects {
		if !accessName(project.Project) {
			return nil, accessCode("identifier")
		}
		if policy.Projects[project.Project] != nil {
			return nil, accessCode("duplicate")
		}
		members := map[string]accessRole{}
		for _, member := range project.Members {
			if !accessID(member.Person) {
				return nil, accessCode("identifier")
			}
			if keys[member.Person] == nil {
				return nil, accessCode("unknown-person")
			}
			if members[member.Person] != roleNone {
				return nil, accessCode("duplicate")
			}
			switch member.Role {
			case "reader":
				members[member.Person] = roleReader
			case "contributor":
				members[member.Person] = roleContributor
			default:
				return nil, accessCode("role")
			}
		}
		policy.Projects[project.Project] = members
	}
	operators := map[string]bool{}
	for _, operator := range wire.Operators {
		if !accessID(operator.Person) {
			return nil, accessCode("identifier")
		}
		if keys[operator.Person] == nil {
			return nil, accessCode("unknown-person")
		}
		if operators[operator.Person] {
			return nil, accessCode("duplicate")
		}
		operators[operator.Person] = true
		rights := map[string]bool{}
		for _, right := range operator.Rights {
			if right != "resource-cancel" {
				return nil, accessCode("right")
			}
			if rights[right] {
				return nil, accessCode("duplicate")
			}
			rights[right] = true
		}
		if rights["resource-cancel"] {
			policy.CancelOperators[operator.Person] = true
		}
	}
	for _, context := range wire.Contexts {
		if !accessID(context.Person) || !accessName(context.Context) || !accessName(context.Backend) {
			return nil, accessCode("identifier")
		}
		if keys[context.Person] == nil {
			return nil, accessCode("unknown-person")
		}
		if _, seen := policy.Contexts[context.Person]; seen {
			return nil, accessCode("duplicate")
		}
		policy.Contexts[context.Person] = accessContext{context.Context, context.Backend}
	}
	for _, device := range wire.RevokedDevices {
		if !accessID(device) {
			return nil, accessCode("identifier")
		}
		if policy.RevokedDevices[device] {
			return nil, accessCode("duplicate")
		}
		policy.RevokedDevices[device] = true
	}
	for _, person := range wire.RevokedPersons {
		if !accessID(person) {
			return nil, accessCode("identifier")
		}
		if keys[person] == nil {
			return nil, accessCode("unknown-person")
		}
		if policy.RevokedPersons[person] {
			return nil, accessCode("duplicate")
		}
		policy.RevokedPersons[person] = true
	}
	return policy, nil
}

// accessDecision is an allowed caller. Via is "" (device), "member" or
// "operator".
type accessDecision struct {
	Person string
	Role   accessRole
	Via    string
}

var accessOperations = map[string]bool{"capabilities": true, "jobs.list": true, "jobs.read": true, "artifacts.read": true,
	"jobs.submit": true, "jobs.control": true, "jobs.cancel": true}

// authorizeAccess resolves the caller from the device alone.
func authorizeAccess(policy *AccessPolicy, device string, now uint64, operation, project string) (accessDecision, error) {
	if !accessOperations[operation] {
		return accessDecision{}, accessCode("operation-unknown")
	}
	enrollment, enrolled := policy.Devices[device]
	if !enrolled {
		return accessDecision{}, accessCode("device-unknown")
	}
	if policy.RevokedDevices[device] {
		return accessDecision{}, accessCode("device-revoked")
	}
	if policy.RevokedPersons[enrollment.Person] {
		return accessDecision{}, accessCode("person-revoked")
	}
	if now < enrollment.Issued {
		return accessDecision{}, accessCode("enrollment-not-yet-valid")
	}
	if now >= enrollment.Expires {
		return accessDecision{}, accessCode("enrollment-expired")
	}
	person := enrollment.Person
	if operation == "capabilities" || operation == "jobs.list" {
		return accessDecision{Person: person}, nil
	}
	members, known := policy.Projects[project]
	if !known {
		return accessDecision{}, accessCode("project-unknown")
	}
	role := members[person]
	if operation == "jobs.cancel" && role == roleNone && policy.CancelOperators[person] {
		return accessDecision{Person: person, Via: "operator"}, nil
	}
	if role == roleNone {
		return accessDecision{}, accessCode("not-a-member")
	}
	if (operation == "jobs.submit" || operation == "jobs.control" || operation == "jobs.cancel") && role != roleContributor {
		return accessDecision{}, accessCode("role-insufficient")
	}
	return accessDecision{Person: person, Role: role, Via: "member"}, nil
}

// personOperations are full-host operations of a caller identified as a
// person (an operator-bound OS account), not through a device.
var personOperations = map[string]bool{"history.read": false, "memory.read": false, "history.publish": true, "memory.write": true, "handoff.continue": true}

// authorizePerson decides a person operation on a project; the value of
// personOperations says whether it needs the contributor role.
func authorizePerson(policy *AccessPolicy, person, operation, project string) (accessDecision, error) {
	contributor, known := personOperations[operation]
	if !known {
		return accessDecision{}, accessCode("operation-unknown")
	}
	if !policy.Persons[person] {
		return accessDecision{}, accessCode("person-unknown")
	}
	if policy.RevokedPersons[person] {
		return accessDecision{}, accessCode("person-revoked")
	}
	members, ok := policy.Projects[project]
	if !ok {
		return accessDecision{}, accessCode("project-unknown")
	}
	role := members[person]
	if role == roleNone {
		return accessDecision{}, accessCode("not-a-member")
	}
	if contributor && role != roleContributor {
		return accessDecision{}, accessCode("role-insufficient")
	}
	return accessDecision{Person: person, Role: role, Via: "member"}, nil
}

func (d accessDecision) String() string {
	return fmt.Sprintf("%s/%s/%s", d.Person, d.Role.name(), d.Via)
}
