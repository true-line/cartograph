"""Score one showcase run against the W10a labels (docs/acceptance/w10a/README.md).

python3 -I score.py <app checkout> <graph.db> [labels.json]

Reads the run's graph (read-only) and the app source only to turn each
labelled call line into a byte range. Prints a JSON report.
"""
import json
import pathlib
import sqlite3
import sys

GAP_REASONS = {
    "computed_url": "request URL is computed",
    "unknown_endpoint_name": "test client endpoint name is not a recovered endpoint",
    "multiple_urls": "endpoint name builds more than one recovered URL",
    "no_endpoint_serves_path": "no recovered endpoint serves this path",
}


def line_ranges(text: bytes):
    starts = [0]
    for i, b in enumerate(text):
        if b == 0x0A:
            starts.append(i + 1)
    starts.append(len(text) + 1)
    return starts


def main():
    app = pathlib.Path(sys.argv[1])
    db = sqlite3.connect(f"file:{sys.argv[2]}?mode=ro", uri=True)
    here = pathlib.Path(__file__).resolve().parent
    labels = json.loads(pathlib.Path(sys.argv[3] if len(sys.argv) > 3 else here / "labels.json").read_text())

    nodes = {i: (l, json.loads(p)) for i, l, p in db.execute("select id, label, props from nodes where label in ('Endpoint','Gap','TestCase')")}
    requests = [(s, d, json.loads(p)) for s, d, p in db.execute("select src, dst, props from edges where label = 'REQUESTS'")]
    tests = {}
    for node_id, (label, _) in nodes.items():
        if label == "TestCase" and node_id.startswith("test:"):
            repo, rest = node_id[len("test:"):].split("@", 1)
            tests[rest.replace("#", "::", 1)] = (repo, node_id)

    sources = {}

    def span(path, line):
        if path not in sources:
            sources[path] = line_ranges((app / path).read_bytes())
        starts = sources[path]
        return starts[line - 1], starts[line] - 1

    def cites(props, path, line):
        lo, hi = span(path, line)
        return any(e["path"] == path and lo <= e["byte_start"] <= hi for e in props["prov"]["evidence"])

    def truth_endpoint(repo, call):
        truth = call["truth"]
        if "::" not in truth:
            return None
        file, func = truth.split("::")
        sym = f"sym:{repo}@{file}#{func}"
        hits = [i for i, (l, p) in nodes.items() if l == "Endpoint" and p.get("handler_sym") == sym and p.get("method") == call["method"]]
        return hits

    report = {"calls": [], "misses": []}
    link_total = link_hit = 0
    prec_total = prec_ok = 0
    gap_total = gap_hit = 0
    redirect_hit = redirect_total = 0
    redirect_extra = []
    edge_labels = {}
    for call in labels["calls"]:
        test_key = call["test"]
        if test_key not in tests:
            report["misses"].append({"call": f"{test_key}:{call['line']}", "reason": "test not recovered as a TestCase"})
            continue
        repo, test_id = tests[test_key]
        path = test_key.split("::")[0]
        mine = [(d, p) for s, d, p in requests if s == test_id and cites(p, path, call["line"])]
        links = [(d, p) for d, p in mine if not d.startswith("gap:")]
        gaps = [(d, p) for d, p in mine if d.startswith("gap:")]
        truths = truth_endpoint(repo, call) or []
        entry = {"call": f"{test_key}:{call['line']}", "expect": call["expect"], "links": [d for d, _ in links], "gaps": [p.get("reason") for _, p in gaps]}
        for d, p in links:
            prec_total += 1
            if d in truths:
                prec_ok += 1
            else:
                report["misses"].append({"call": entry["call"], "reason": f"links to {d}, truth {call['truth']}"})
            key = (test_id, d)
            lab = edge_labels.setdefault(key, {"statuses": set(), "keys": set(), "redirect": False, "props": p})
            if call["expect"].startswith("link"):
                lab["statuses"].update(call["statuses"] if call["expect"] == "link" else [])
                lab["keys"].update(call["keys"] if call["expect"] == "link" else [])
                lab["redirect"] |= call["expect"] == "link:follows_redirects"
        if call["expect"].startswith("link"):
            link_total += 1
            ok = any(d in truths for d, _ in links)
            link_hit += ok
            if not ok:
                report["misses"].append({"call": entry["call"], "reason": "no REQUESTS edge to the truth endpoint citing this line" + (f"; Gap: {entry['gaps']}" if gaps else "")})
            if call["expect"] == "link:follows_redirects":
                redirect_total += 1
                good = [p for d, p in links if d in truths and p.get("follows_redirects") is True and not p.get("statuses") and not p.get("keys")]
                redirect_hit += bool(good)
        else:
            gap_total += 1
            want = GAP_REASONS[call["expect"].split(":", 1)[1]]
            ok = not links and any(p.get("reason") == want for _, p in gaps)
            gap_hit += ok
            if not ok:
                report["misses"].append({"call": entry["call"], "reason": f"expected Gap '{want}'; got links {entry['links']} gaps {entry['gaps']}"})
        report["calls"].append(entry)

    st_tp = st_pred = st_lab = k_tp = k_pred = k_lab = 0
    for (test_id, d), lab in edge_labels.items():
        p = lab["props"]
        if p.get("follows_redirects") and not lab["redirect"]:
            redirect_extra.append(f"{test_id} -> {d}")
        pred_s, pred_k = set(p.get("statuses", [])), set(p.get("keys", []))
        st_tp += len(pred_s & lab["statuses"]); st_pred += len(pred_s); st_lab += len(lab["statuses"])
        k_tp += len(pred_k & lab["keys"]); k_pred += len(pred_k); k_lab += len(lab["keys"])
        for s in sorted(pred_s - lab["statuses"]):
            report["misses"].append({"edge": f"{test_id} -> {d}", "reason": f"status {s} not labelled"})
        for s in sorted(lab["statuses"] - pred_s):
            report["misses"].append({"edge": f"{test_id} -> {d}", "reason": f"labelled status {s} missing"})
        for k in sorted(pred_k - lab["keys"]):
            report["misses"].append({"edge": f"{test_id} -> {d}", "reason": f"key {k} not labelled"})
        for k in sorted(lab["keys"] - pred_k):
            report["misses"].append({"edge": f"{test_id} -> {d}", "reason": f"labelled key {k} missing"})

    ratio = lambda a, b: f"{a}/{b}"
    report["scores"] = {
        "link_recall": {"value": ratio(link_hit, link_total), "pass": link_hit >= link_total - 1},
        "link_precision": {"value": ratio(prec_ok, prec_total), "pass": prec_ok == prec_total},
        "gap_accuracy": {"value": ratio(gap_hit, gap_total), "pass": gap_hit == gap_total},
        "redirects": {"value": ratio(redirect_hit, redirect_total), "extra": redirect_extra, "pass": redirect_hit == redirect_total and not redirect_extra},
        "status_precision": {"value": ratio(st_tp, st_pred), "pass": st_tp == st_pred},
        "status_recall": {"value": ratio(st_tp, st_lab), "pass": st_lab == 0 or st_tp / st_lab >= 0.9},
        "key_precision": {"value": ratio(k_tp, k_pred), "pass": k_tp == k_pred},
        "key_recall": {"value": ratio(k_tp, k_lab), "pass": k_lab == 0 or k_tp / k_lab >= 0.9},
    }
    report["pass"] = all(s["pass"] for s in report["scores"].values())
    print(json.dumps(report, indent=1))


main()
