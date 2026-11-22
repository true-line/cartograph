"""Score one showcase run against the W11 expected set (docs/acceptance/w11/README.md).

python3 -I score.py <run output dir> [expected.json]

Reads `best-effort/journeys.md` and `fetches.json` from the run. A screen
step matches a candidate screen by its route (rules in SCREEN_ROUTES); a
client-call step matches a candidate client call or goal by method and path,
query dropped. Prints a JSON report.
"""
import json
import pathlib
import re
import sys

SCREEN_ROUTES = {
    "J1": re.compile(r"api[-_ ]?key|onboarding", re.I),
    "J2": re.compile(r"^/main/?$", re.I),
    "J6": re.compile(r"new[-_ ]?alias", re.I),
}


def norm(call):
    method, _, path = call.strip().partition(" ")
    return f"{method} {path.split('?')[0].rstrip('/') or '/'}"


def candidates(text):
    out = []
    for block in re.split(r"\n## ", text)[1:]:
        title = block.splitlines()[0]
        if "### Steps" not in block:
            continue
        steps = []
        table = block.split("### Steps", 1)[1].split("###", 1)[0]
        for row in table.splitlines():
            cells = [c.strip() for c in row.strip().strip("|").split("|")]
            if len(cells) < 5 or not cells[0].isdigit():
                continue
            screen = cells[1].strip("`")
            calls = [norm(c) for c in cells[3].split(",") if c.strip() and c.strip() != "—"]
            steps.append({"screen": screen, "calls": calls})
        goal = re.search(r"- Endpoint: (\S+ \S+)", block)
        # An ordered result step is the last row, after the goal call.
        ordered = re.search(r"^- Result: ordered", block, re.M) is not None
        out.append({"title": title, "steps": steps, "goal": norm(goal.group(1)) if goal else None, "ordered": ordered})
    return out


def score(expected, cand):
    found = {}
    for step in expected["steps"]:
        sid = step["id"]
        if step["kind"] == "screen":
            for i, s in enumerate(cand["steps"]):
                if sid in SCREEN_ROUTES and SCREEN_ROUTES[sid].search(s["screen"]):
                    found[sid] = {"at": (i, -1), "matched": s["screen"]}
                    break
        else:
            wanted = [norm(e) for e in step.get("endpoint_any", [step["endpoint"]])]
            # The goal is made on the last screen before any ordered result
            # step: after that screen's other calls, before the result step.
            if cand["goal"] in wanted:
                at = len(cand["steps"]) - 1 - cand["ordered"]
                found[sid] = {"at": (at, 1), "matched": cand["goal"]}
                continue
            for i, s in enumerate(cand["steps"]):
                hit = next((c for c in s["calls"] if c in wanted), None)
                if hit:
                    found[sid] = {"at": (i, 0), "matched": hit}
                    break
    return found


def main():
    run = pathlib.Path(sys.argv[1])
    here = pathlib.Path(__file__).resolve().parent
    expected = json.loads(pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else here / "expected.json").read_text())
    cands = candidates((run / "best-effort" / "journeys.md").read_text())
    fetched = {norm(f"{f['method']} {f['dst'].split(':', 2)[-1]}") for f in json.loads((run / "fetches.json").read_text()) if f.get("dst", "").startswith("ep:")}
    required = [s["id"] for s in expected["steps"] if s["required"]]
    scored = [(c, score(expected, c)) for c in cands]
    # The candidate with most required steps; on a tie, the one whose goal is
    # a required call, so the alternate path never wins by listing order.
    required_goals = {norm(e) for s in expected["steps"] if s["required"] and s["kind"] == "client_call" for e in s.get("endpoint_any", [s["endpoint"]])}
    best, found = max(scored, key=lambda cf: (sum(1 for r in required if r in cf[1]), cf[0]["goal"] in required_goals))
    present = [r for r in expected["order"] if r in found]
    # Strictly increasing: two calls listed on one screen carry no order
    # between them, so a tie is unproven, never in order.
    at = [found[r]["at"] for r in present]
    order_ok = all(a < b for a, b in zip(at, at[1:]))
    calls = {s["id"]: s for s in expected["steps"] if s["kind"] == "client_call"}
    endpoint_rows = {sid: found[sid]["matched"] in fetched for sid in calls if sid in found}
    expected_calls = {norm(e) for s in calls.values() for e in s.get("endpoint_any", [s["endpoint"]])}
    extra = sorted({c for s in best["steps"] for c in s["calls"]} - expected_calls)
    alternate = [s["id"] for s in expected["steps"] if s.get("alternate_of")]
    # The alternate is never the implemented path (the goal) of the journey
    # that carries the step it is an alternate of.
    alt_ok = not any(a in found and found[a]["matched"] == best["goal"] and calls[a]["alternate_of"] in found for a in alternate)
    report = {
        "candidate": best["title"],
        "candidates_read": len(cands),
        "steps": {s["id"]: ({"found": found[s["id"]]["matched"], "position": found[s["id"]]["at"][0]} if s["id"] in found else {"found": None}) for s in expected["steps"]},
        "missing_required": [r for r in required if r not in found],
        "extra_calls": extra,
        "scores": {
            "required_steps": {"value": f"{len(present)}/{len(required)}", "pass": len(present) == len(required)},
            "order": {"value": order_ok, "pass": order_ok and len(present) == len(required)},
            "alternate_path": {"value": alt_ok, "pass": alt_ok},
            "endpoints": {"value": f"{sum(endpoint_rows.values())}/{len(endpoint_rows)}", "rows": endpoint_rows, "pass": all(endpoint_rows.values())},
        },
    }
    report["result"] = "complete" if not report["missing_required"] else "partial"
    report["pass"] = all(s["pass"] for s in report["scores"].values())
    print(json.dumps(report, indent=1))


main()
