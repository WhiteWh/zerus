#!/usr/bin/env python3
"""Private account-edit fixture. Never reads hgs state or credentials."""
import json
import sys
from pathlib import Path

root = Path(__file__).parent
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as log:
    log.write(json.dumps(args) + "\n")
if args[0].startswith("@"):
    assert args.pop(0) == "@studio"
assert args[:2] == ["--expect-machine", "11111111-1111-4111-8111-111111111111"]
assert args[2] == "account"
catalog = json.loads((root / "catalog.json").read_text())
if args[3] == "ls":
    print(json.dumps(catalog))
    sys.exit()
if args[3] == "add":
    assert args[args.index("--revision")+1] == catalog["revision"]
    profile={"id":args[4],"provider":args[args.index("--provider")+1],"label":args[args.index("--label")+1],"home":"/accounts/new","auth_revision":"new","installed":True}
    catalog["profiles"].append(profile)
elif args[3] == "restore":
    profile=next(p for p in catalog["removed_profiles"] if p["id"]==args[4])
else:
    profile = next(p for p in catalog["profiles"] if p["id"] == args[4])
if args[3] == "model-settings":
    print(json.dumps({"account_id": profile["id"], "provider": profile["provider"], "revision": catalog["revision"],
        "models": [{"id": "model-a", "label": "Example model", "effort_options": ["low", "high"]}],
        "effort_supported": True, "defaults": profile.get("model_defaults", {"model": "", "effort": ""}),
        "provider_defaults": {"model": "model-a", "effort": "low"}}))
    sys.exit()
assert args[args.index("--revision") + 1] == catalog["revision"]
if args[3] == "add":
    pass
elif args[3] == "restore":
    catalog["removed_profiles"].remove(profile);catalog["profiles"].append(profile)
elif args[3] == "set-key":
    body=json.load(sys.stdin);assert body["api_key"]=="fixture-secret"
    profile["label"]=body.get("label",profile["label"])
elif args[3] == "rename":
    profile["label"] = args[args.index("--label") + 1]
elif args[3] == "default":
    profile["is_default"] = True
elif args[3] == "rm":
    catalog.setdefault("removed_profiles", []).append(profile)
    catalog["profiles"].remove(profile)
elif args[3] == "permissions":
    mode = args[args.index("--mode") + 1]
    assert mode in ("provider", "bypass")
    profile["permission_mode"] = mode
elif args[3] == "model-defaults":
    model, effort = args[args.index("--model") + 1], args[args.index("--effort") + 1]
    assert (not model and not effort) or (model == "model-a" and effort in ("", "low", "high"))
    profile["model_defaults"] = {"model": model, "effort": effort}
else:
    raise AssertionError(args)
catalog["revision"] = "r" + str(int(catalog["revision"][1:]) + 1)
(root / "catalog.json").write_text(json.dumps(catalog))
if (root / "uncertain").exists():
    print("Synthetic uncertain result", file=sys.stderr)
    sys.exit(7)
print(json.dumps(catalog))
