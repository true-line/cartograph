"""Tabulate one scored run against expected.json (AC-0670).

    python3 -I score.py scores.json

scores.json is written by the reviewer:

    {"reviewer": "...", "run": {"graph_content_hash": "...", "curated_revision": "...",
                                "role_definition": "...", "model": "..."},
     "claims": [{"section": "1.5", "draft_id": "...", "claim": 0,
                 "verdict": "supported" | "unsupported", "expected": ["R1"], "note": ""}]}

A claim is one printed claim of an accepted draft. `expected` lists the
expected items it states, and may be empty. An expected item no supported
claim lists is missing. Nothing here reads a model or the graph.
"""
import json
import pathlib
import sys

here = pathlib.Path(__file__).parent
expected = json.loads((here / "expected.json").read_text())
scores = json.loads(pathlib.Path(sys.argv[1]).read_text())
ids = {item["id"]: item for item in expected["expected"]}
errors = []
if not scores.get("reviewer"):
    errors.append("no reviewer named")
covered = set()
table = {section: {"supported": 0, "unsupported": 0} for section in expected["sections"]}
for position, claim in enumerate(scores.get("claims", [])):
    where = f"claims[{position}]"
    if claim.get("section") not in table:
        errors.append(f"{where}: section is not in the pilot")
        continue
    if claim.get("verdict") not in ("supported", "unsupported"):
        errors.append(f"{where}: verdict must be supported or unsupported")
        continue
    table[claim["section"]][claim["verdict"]] += 1
    for item in claim.get("expected", []):
        if item not in ids:
            errors.append(f"{where}: unknown expected id {item}")
        elif claim["verdict"] == "supported":
            covered.add(item)
if errors:
    sys.exit("\n".join(errors))
print("| Section | Printed claims | Supported | Unsupported | Expected | Missing |")
print("|---|---|---|---|---|---|")
for section in expected["sections"]:
    wanted = [item for item in ids.values() if item["section"] == section]
    missing = [item["id"] for item in wanted if item["id"] not in covered]
    row = table[section]
    print(
        f"| {section} | {row['supported'] + row['unsupported']} | {row['supported']} "
        f"| {row['unsupported']} | {len(wanted)} | {len(missing)}"
        f"{' (' + ', '.join(missing) + ')' if missing else ''} |"
    )
