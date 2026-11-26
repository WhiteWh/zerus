#!/usr/bin/env python3
"""Write (or --check) the zerus-host-access-v1 fixtures from synthetic keys.

Every key and seed is fixture data derived from a public label. Nothing here is
a production key, and no production code path reads these files.
"""
import argparse
import base64
import copy
import hashlib
import json
import sys
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"
CONTRACT = "zerus-host-access-v1"
NOW = 1795000000
DAY = 86400
L = 2**252 + 27742317777372353535851937790883648493


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def b64(raw):
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode()


class Person:
    def __init__(self, name):
        self.name = name
        self.seed = hashlib.sha256(f"{CONTRACT} synthetic fixture person {name}".encode()).digest()
        self.key = Ed25519PrivateKey.from_private_bytes(self.seed)
        self.public = self.key.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
        self.id = hashlib.sha256(self.public).hexdigest()

    def row(self):
        return {"person": self.id, "public_key": b64(self.public), "label": f"Synthetic {self.name}"}


def statement(host, person, device, issued, expires, newline="\n"):
    lines = [f"{CONTRACT}/device-enrollment", f"host={host}", f"person={person}",
             f"device={device}", f"issued={issued}", f"expires={expires}"]
    return "".join(line + newline for line in lines).encode()


def enroll(signer, host, person, device, issued, expires, newline="\n"):
    signature = signer.key.sign(statement(host, person, device, issued, expires, newline))
    return {"device": device, "person": person, "issued": issued, "expires": expires,
            "signature": b64(signature)}


HOST = sha("synthetic host H1")
OTHER_HOST = sha("synthetic host H2")
alice, bob, carol, mallory = (Person(n) for n in ("alice", "bob", "carol", "mallory"))
DEV = {name: sha(f"synthetic device {name}") for name in (
    "alice-laptop", "alice-desktop", "alice-future", "alice-expired", "bob-laptop",
    "bob-old", "carol-console", "stranger")}
ISSUED, EXPIRES = NOW - DAY, NOW + 30 * DAY


def base_policy():
    return {
        "contract": CONTRACT,
        "host": HOST,
        "revision": 1,
        "persons": [alice.row(), bob.row(), carol.row()],
        "devices": [
            enroll(alice, HOST, alice.id, DEV["alice-laptop"], ISSUED, EXPIRES),
            enroll(alice, HOST, alice.id, DEV["alice-desktop"], ISSUED, EXPIRES),
            enroll(alice, HOST, alice.id, DEV["alice-future"], NOW + DAY, EXPIRES),
            enroll(alice, HOST, alice.id, DEV["alice-expired"], NOW - 2 * DAY, NOW),
            enroll(bob, HOST, bob.id, DEV["bob-laptop"], ISSUED, EXPIRES),
            enroll(bob, HOST, bob.id, DEV["bob-old"], ISSUED, EXPIRES),
            enroll(carol, HOST, carol.id, DEV["carol-console"], ISSUED, EXPIRES),
        ],
        "projects": [
            {"project": "proj-shared", "members": [
                {"person": alice.id, "role": "contributor"}, {"person": bob.id, "role": "contributor"}]},
            {"project": "proj-alice", "members": [{"person": alice.id, "role": "contributor"}]},
            {"project": "proj-read", "members": [
                {"person": bob.id, "role": "contributor"}, {"person": alice.id, "role": "reader"}]},
        ],
        "operators": [{"person": carol.id, "rights": ["resource-cancel"]}],
        "contexts": [
            {"person": alice.id, "context": "alice-worker", "backend": "windows-user-sid"},
            {"person": bob.id, "context": "bob-worker", "backend": "windows-user-sid"},
        ],
        "revoked_devices": [DEV["bob-old"]],
        "revoked_persons": [],
    }


def text(policy):
    return json.dumps(policy, separators=(",", ":"), ensure_ascii=False)


def mutate(change):
    policy = base_policy()
    change(policy)
    return text(policy)


def flip_signature(entry):
    raw = bytearray(base64.urlsafe_b64decode(entry["signature"] + "=="))
    raw[10] ^= 1
    entry["signature"] = b64(bytes(raw))


def non_canonical_s(entry):
    raw = bytearray(base64.urlsafe_b64decode(entry["signature"] + "=="))
    s = int.from_bytes(raw[32:], "little") + L
    raw[32:] = s.to_bytes(32, "little")
    entry["signature"] = b64(bytes(raw))


def weak_key_person(policy):
    weak = bytes(32)  # the identity point: small order
    policy["persons"].append({"person": hashlib.sha256(weak).hexdigest(), "public_key": b64(weak), "label": "Weak"})


def noncanonical_key_person(policy):
    raw = bytes([0xED] + [0xFF] * 30 + [0x7F])  # y = p: non-canonical encoding of 0
    policy["persons"].append({"person": hashlib.sha256(raw).hexdigest(), "public_key": b64(raw), "label": "Noncanonical"})


P = 2**255 - 19
D_CURVE = (-121665 * pow(121666, P - 2, P)) % P


def undecompressible(count):
    """Canonical encodings whose y has no x on the curve (fixture math only)."""
    found, y = [], 2
    while len(found) < count:
        u, v = (y * y - 1) % P, (D_CURVE * y * y + 1) % P
        x2 = u * pow(v, P - 2, P) % P
        if x2 and pow(x2, (P - 1) // 2, P) == P - 1:
            found.append(y.to_bytes(32, "little"))
        y += 1
    return found


def off_curve_person(raw, sign=False):
    def change(policy):
        key = bytearray(raw)
        if sign:
            key[31] |= 0x80
        key = bytes(key)
        policy["persons"].append({"person": hashlib.sha256(key).hexdigest(), "public_key": b64(key), "label": "Off curve"})
    return change


def invalid_cases():
    base = text(base_policy())
    cases = [
        ("too-large", {"text": base, "pad_to_bytes": 1048577}, "too-large"),
        ("unknown top-level member", {"text": mutate(lambda p: p.update(owner=alice.id))}, "malformed"),
        ("unknown device member", {"text": mutate(lambda p: p["devices"][0].update(role="contributor"))}, "malformed"),
        ("duplicate JSON member", {"text": base.replace('"revision":1', '"revision":1,"revision":2', 1)}, "malformed"),
        ("trailing data", {"text": base + "{}"}, "malformed"),
        ("string revision", {"text": mutate(lambda p: p.update(revision="1"))}, "malformed"),
        ("negative revision", {"text": mutate(lambda p: p.update(revision=-1))}, "malformed"),
        ("array document", {"text": "[" + base + "]"}, "malformed"),
        ("case-variant member", {"text": base.replace('"revision":', '"Revision":', 1)}, "malformed"),
        ("null list", {"text": mutate(lambda p: p.update(revoked_persons=None))}, "malformed"),
        ("null label", {"text": mutate(lambda p: p["persons"][0].update(label=None))}, "malformed"),
        ("fractional revision", {"text": base.replace('"revision":1', '"revision":1.0', 1)}, "malformed"),
        ("wrong contract", {"text": mutate(lambda p: p.update(contract="zerus-host-access-v2"))}, "contract"),
        ("missing contract", {"text": mutate(lambda p: p.pop("contract"))}, "contract"),
        ("uppercase host", {"text": mutate(lambda p: p.update(host=HOST.upper()))}, "host"),
        ("substituted host", {"text": mutate(lambda p: p.update(host=OTHER_HOST))}, "host-mismatch"),
        ("zero revision", {"text": mutate(lambda p: p.update(revision=0))}, "revision"),
        ("revision above 2^53-1", {"text": mutate(lambda p: p.update(revision=2**53))}, "revision"),
        ("too many operators", {"text": mutate(lambda p: p.update(operators=[{"person": "x", "rights": []}] * 65))}, "limits"),
        ("too many rights", {"text": mutate(lambda p: p["operators"][0].update(rights=["resource-cancel"] * 5))}, "limits"),
        ("label with LF", {"text": mutate(lambda p: p["persons"][0].update(label="Alice\nAdmin"))}, "identifier"),
        ("label with CR", {"text": mutate(lambda p: p["persons"][0].update(label="Alice\r"))}, "identifier"),
        ("empty label", {"text": mutate(lambda p: p["persons"][0].update(label=""))}, "identifier"),
        ("uppercase device", {"text": mutate(lambda p: p["devices"][0].update(device=DEV["alice-laptop"].upper()))}, "identifier"),
        ("bad project name", {"text": mutate(lambda p: p["projects"][0].update(project="Proj"))}, "identifier"),
        ("long context name", {"text": mutate(lambda p: p["contexts"][0].update(context="c" * 65))}, "identifier"),
        ("weak person key", {"text": mutate(weak_key_person)}, "person-key"),
        ("non-canonical person key", {"text": mutate(noncanonical_key_person)}, "person-key"),
        ("undecompressible person key y=2", {"text": mutate(off_curve_person(undecompressible(1)[0]))}, "person-key"),
        ("undecompressible person key, next y", {"text": mutate(off_curve_person(undecompressible(2)[1]))}, "person-key"),
        ("undecompressible person key, sign bit", {"text": mutate(off_curve_person(undecompressible(2)[1], True))}, "person-key"),
        ("short person key", {"text": mutate(lambda p: p["persons"][0].update(public_key=b64(alice.public[:31])))}, "person-key"),
        ("padded person key", {"text": mutate(lambda p: p["persons"][0].update(public_key=b64(alice.public) + "="))}, "person-key"),
        ("claimed person ID", {"text": mutate(lambda p: p["persons"][0].update(person=bob.id))}, "person-id"),
        ("duplicate person", {"text": mutate(lambda p: p["persons"].append(alice.row()))}, "duplicate"),
        ("duplicate device", {"text": mutate(lambda p: p["devices"].append(copy.deepcopy(p["devices"][0])))}, "duplicate"),
        ("device for unknown person", {"text": mutate(lambda p: p["devices"].append(
            enroll(mallory, HOST, mallory.id, DEV["stranger"], ISSUED, EXPIRES)))}, "unknown-person"),
        ("empty enrollment window", {"text": mutate(lambda p: p["devices"].__setitem__(0,
            enroll(alice, HOST, alice.id, DEV["alice-laptop"], ISSUED, ISSUED)))}, "window"),
        ("tampered signature", {"text": mutate(lambda p: flip_signature(p["devices"][0]))}, "signature"),
        ("non-canonical signature S", {"text": mutate(lambda p: non_canonical_s(p["devices"][0]))}, "signature"),
        ("padded signature", {"text": mutate(lambda p: p["devices"][0].update(signature=p["devices"][0]["signature"] + "="))}, "signature"),
        ("CRLF statement", {"text": mutate(lambda p: p["devices"].__setitem__(0,
            enroll(alice, HOST, alice.id, DEV["alice-laptop"], ISSUED, EXPIRES, "\r\n")))}, "signature"),
        ("enrollment for another host", {"text": mutate(lambda p: p["devices"].__setitem__(0,
            enroll(alice, OTHER_HOST, alice.id, DEV["alice-laptop"], ISSUED, EXPIRES)))}, "signature"),
        ("signature reused for another device", {"text": mutate(lambda p: p["devices"][1].update(
            signature=p["devices"][0]["signature"]))}, "signature"),
        ("device signed by another person", {"text": mutate(lambda p: p["devices"].__setitem__(0,
            enroll(bob, HOST, alice.id, DEV["alice-laptop"], ISSUED, EXPIRES)))}, "signature"),
        ("extended expiry", {"text": mutate(lambda p: p["devices"][0].update(expires=EXPIRES + DAY))}, "signature"),
        ("duplicate project", {"text": mutate(lambda p: p["projects"].append(copy.deepcopy(p["projects"][1])))}, "duplicate"),
        ("member unknown person", {"text": mutate(lambda p: p["projects"][0]["members"].append(
            {"person": mallory.id, "role": "reader"}))}, "unknown-person"),
        ("duplicate member", {"text": mutate(lambda p: p["projects"][0]["members"].append(
            {"person": alice.id, "role": "reader"}))}, "duplicate"),
        ("unknown role", {"text": mutate(lambda p: p["projects"][0]["members"][0].update(role="owner"))}, "role"),
        ("duplicate operator", {"text": mutate(lambda p: p["operators"].append(copy.deepcopy(p["operators"][0])))}, "duplicate"),
        ("unknown right", {"text": mutate(lambda p: p["operators"][0].update(rights=["read-results"]))}, "right"),
        ("duplicate right", {"text": mutate(lambda p: p["operators"][0].update(rights=["resource-cancel"] * 2))}, "duplicate"),
        ("context unknown person", {"text": mutate(lambda p: p["contexts"].append(
            {"person": mallory.id, "context": "m", "backend": "windows-user-sid"}))}, "unknown-person"),
        ("second context for a person", {"text": mutate(lambda p: p["contexts"].append(
            {"person": alice.id, "context": "alice-2", "backend": "windows-user-sid"}))}, "duplicate"),
        ("duplicate revoked device", {"text": mutate(lambda p: p["revoked_devices"].append(DEV["bob-old"]))}, "duplicate"),
        ("revoked unknown person", {"text": mutate(lambda p: p["revoked_persons"].append(mallory.id))}, "unknown-person"),
    ]
    return [{"name": name, **body, "expect": code} for name, body, code in cases]


def decisions():
    def allow(person, role="", via=""):
        return {"allow": True, "person": person.id, "role": role, "via": via}

    def refuse(code):
        return {"allow": False, "code": code}

    rows = [
        ("enrolled device reads capabilities", "base", "alice-laptop", "capabilities", "", allow(alice)),
        ("second device of the same person", "base", "alice-desktop", "jobs.list", "", allow(alice)),
        ("unknown operation first", "base", "stranger", "jobs.delete", "proj-shared", refuse("operation-unknown")),
        ("unknown device", "base", "stranger", "capabilities", "", refuse("device-unknown")),
        ("revoked device", "base", "bob-old", "jobs.list", "", refuse("device-revoked")),
        ("future enrollment", "base", "alice-future", "capabilities", "", refuse("enrollment-not-yet-valid")),
        ("expiry is exclusive", "base", "alice-expired", "capabilities", "", refuse("enrollment-expired")),
        ("contributor submits", "base", "alice-laptop", "jobs.submit", "proj-shared", allow(alice, "contributor", "member")),
        ("reader cannot submit", "base", "alice-laptop", "jobs.submit", "proj-read", refuse("role-insufficient")),
        ("reader reads", "base", "alice-laptop", "jobs.read", "proj-read", allow(alice, "reader", "member")),
        ("reader reads artifacts", "base", "alice-laptop", "artifacts.read", "proj-read", allow(alice, "reader", "member")),
        ("reader cannot control", "base", "alice-laptop", "jobs.control", "proj-read", refuse("role-insufficient")),
        ("non-member read", "base", "bob-laptop", "jobs.read", "proj-alice", refuse("not-a-member")),
        ("empty project", "base", "alice-laptop", "jobs.submit", "", refuse("project-unknown")),
        ("unknown project", "base", "alice-laptop", "jobs.submit", "proj-none", refuse("project-unknown")),
        ("member cancels", "base", "alice-laptop", "jobs.cancel", "proj-shared", allow(alice, "contributor", "member")),
        ("operator cancels", "base", "carol-console", "jobs.cancel", "proj-shared", allow(carol, "", "operator")),
        ("operator cannot read", "base", "carol-console", "jobs.read", "proj-shared", refuse("not-a-member")),
        ("non-member cannot cancel", "base", "bob-laptop", "jobs.cancel", "proj-alice", refuse("not-a-member")),
        ("reader cannot cancel", "base", "alice-laptop", "jobs.cancel", "proj-read", refuse("role-insufficient")),
        ("revoked person", "bob-revoked", "bob-laptop", "capabilities", "", refuse("person-revoked")),
        ("device revocation precedes person", "bob-revoked", "bob-old", "capabilities", "", refuse("device-revoked")),
        ("other person unaffected", "bob-revoked", "alice-laptop", "jobs.submit", "proj-shared", allow(alice, "contributor", "member")),
    ]
    return [{"name": n, "policy": p, "device": DEV[d], "now": NOW, "operation": o, "project": pr, "expect": e}
            for n, p, d, o, pr, e in rows]


def person_decisions():
    """Full-host operations for callers identified as a person (an OS account
    bound to a policy context), not through a device."""
    def allow(person, role):
        return {"allow": True, "person": person.id, "role": role, "via": "member"}

    def refuse(code):
        return {"allow": False, "code": code}

    rows = [
        ("unknown operation first", "base", mallory, "jobs.submit", "proj-shared", refuse("operation-unknown")),
        ("unknown person", "base", mallory, "history.read", "proj-shared", refuse("person-unknown")),
        ("member reads history", "base", alice, "history.read", "proj-shared", allow(alice, "contributor")),
        ("reader reads memory", "base", alice, "memory.read", "proj-read", allow(alice, "reader")),
        ("reader cannot publish history", "base", alice, "history.publish", "proj-read", refuse("role-insufficient")),
        ("reader cannot write memory", "base", alice, "memory.write", "proj-read", refuse("role-insufficient")),
        ("reader cannot continue a handoff", "base", alice, "handoff.continue", "proj-read", refuse("role-insufficient")),
        ("contributor publishes", "base", bob, "history.publish", "proj-shared", allow(bob, "contributor")),
        ("contributor writes memory", "base", bob, "memory.write", "proj-read", allow(bob, "contributor")),
        ("contributor continues", "base", alice, "handoff.continue", "proj-shared", allow(alice, "contributor")),
        ("non-member", "base", bob, "history.read", "proj-alice", refuse("not-a-member")),
        ("operator is not a member", "base", carol, "history.read", "proj-shared", refuse("not-a-member")),
        ("unknown project", "base", alice, "history.read", "proj-none", refuse("project-unknown")),
        ("empty project", "base", alice, "memory.read", "", refuse("project-unknown")),
        ("revoked person", "bob-revoked", bob, "history.read", "proj-shared", refuse("person-revoked")),
        ("other person unaffected", "bob-revoked", alice, "history.read", "proj-shared", allow(alice, "contributor")),
    ]
    return [{"name": n, "policy": p, "person": who.id, "operation": o, "project": pr, "expect": e}
            for n, p, who, o, pr, e in rows]


def account_refs():
    def ref(person, context, provider, account):
        return {"person": person.id, "context": context, "provider": provider, "account": account}

    a = ref(alice, "alice-worker", "codex", "native-codex")
    b = ref(bob, "bob-worker", "codex", "native-codex")
    return {
        "keys": [{"ref": a, "key": f"zerus-account-v1:{alice.id}/alice-worker/codex/native-codex"},
                 {"ref": b, "key": f"zerus-account-v1:{bob.id}/bob-worker/codex/native-codex"}],
        "distinct": [[a, b], [a, ref(alice, "alice-other", "codex", "native-codex")],
                     [a, ref(alice, "alice-worker", "claude", "native-codex")]],
        "invalid": [ref(alice, "alice-worker", "codex", "-x"), ref(alice, "alice-worker", "codex", "a/b"),
                    ref(alice, "Alice", "codex", "work"), ref(alice, "alice-worker", "Codex", "work"),
                    {"person": "alice", "context": "alice-worker", "provider": "codex", "account": "work"}],
    }


MAX_REQUEST_LIFETIME = 400 * DAY
MAX_REQUEST_AGE = 7 * DAY


def request(signer, device, issued, expires, host=HOST, person=None, label=None, kind="device-enrollment-request",
            sign_host=None, sign_expires=None, key_of=None):
    """A device enrollment request: the person's signed statement for one host."""
    person = person if person is not None else signer.id
    statement_host = sign_host if sign_host is not None else host
    statement_expires = sign_expires if sign_expires is not None else expires
    signature = signer.key.sign(statement(statement_host, signer.id, device, issued, statement_expires))
    owner = key_of or signer
    return {"contract": CONTRACT, "kind": kind, "host": host, "person": person, "public_key": b64(owner.public),
            "label": label if label is not None else f"Synthetic {signer.name}", "device": device,
            "issued": issued, "expires": expires, "signature": b64(signature)}


def enrollment_requests():
    new = sha("synthetic device mallory-new")
    issued, expires = NOW - 60, NOW + 30 * DAY
    cases = [
        ("valid", "base", request(mallory, new, issued, expires), ""),
        ("known person adds a device", "base", request(alice, sha("synthetic device alice-tablet"), issued, expires), ""),
        ("wrong kind", "base", request(mallory, new, issued, expires, kind="policy"), "request-contract"),
        ("other host", "base", request(mallory, new, issued, expires, host=OTHER_HOST, sign_host=OTHER_HOST), "request-host-mismatch"),
        ("body person claim", "base", request(mallory, new, issued, expires, person=alice.id), "request-identity"),
        ("key of another person", "base", request(mallory, new, issued, expires, key_of=bob), "request-identity"),
        ("empty label", "base", request(mallory, new, issued, expires, label=""), "request-identity"),
        ("bad device id", "base", request(mallory, "ABC", issued, expires), "request-identity"),
        ("inverted window", "base", request(mallory, new, expires, issued), "request-window"),
        ("too long", "base", request(mallory, new, issued, issued + MAX_REQUEST_LIFETIME + 1), "request-window"),
        ("stale", "base", request(mallory, new, NOW - MAX_REQUEST_AGE - 1, expires), "request-stale"),
        ("issued in the future", "base", request(mallory, new, NOW + 301, expires), "request-stale"),
        ("expired", "base", request(mallory, new, NOW - 2 * DAY, NOW), "request-stale"),
        ("edited expiry", "base", request(mallory, new, issued, expires + DAY, sign_expires=expires), "request-signature"),
        ("revoked device", "base", request(bob, DEV["bob-old"], issued, expires), "request-device-revoked"),
        ("revoked person", "bob-revoked", request(bob, sha("synthetic device bob-new"), issued, expires), "request-person-revoked"),
        ("duplicate device", "base", request(alice, DEV["alice-laptop"], issued, expires), "request-duplicate-device"),
    ]
    return [{"name": n, "policy": policy, "request": r, "code": code} for n, policy, r, code in cases]


def build():
    revoked = base_policy()
    revoked["revision"] = 2
    revoked["revoked_persons"] = [bob.id]
    return {
        "policies.json": {
            "contract": CONTRACT,
            "host": HOST,
            "valid": [{"name": "base", "text": text(base_policy())},
                      {"name": "bob-revoked", "text": text(revoked)}],
            "invalid": invalid_cases(),
        },
        "decisions.json": {"contract": CONTRACT, "host": HOST, "cases": decisions()},
        "person-decisions.json": {"contract": CONTRACT, "host": HOST, "cases": person_decisions()},
        "enrollment-requests.json": {"contract": CONTRACT, "host": HOST, "now": NOW, "cases": enrollment_requests()},
        "account-refs.json": {"contract": CONTRACT, **account_refs()},
        "synthetic-keys.json": {
            "warning": "Synthetic fixture keys derived from public labels. Never use outside tests.",
            "contract": CONTRACT, "host": HOST, "now": NOW,
            "persons": [{"name": p.name, "seed": p.seed.hex(), "person": p.id, "public": b64(p.public)}
                        for p in (alice, bob, carol, mallory)],
            "devices": DEV,
        },
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify committed fixtures instead of writing")
    args = parser.parse_args()
    stale = []
    for name, value in build().items():
        data = (json.dumps(value, indent=1, ensure_ascii=False) + "\n").encode()
        path = FIXTURES / name
        if args.check:
            if not path.is_file() or path.read_bytes() != data:
                stale.append(name)
        else:
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(data)
    if stale:
        print("stale fixtures: " + ", ".join(stale), file=sys.stderr)
        return 1
    print("fixtures " + ("match" if args.check else "written"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
