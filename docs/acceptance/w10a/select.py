import ast, hashlib, pathlib, sys
root = pathlib.Path(sys.argv[1])
rows = []
for f in sorted(root.glob("tests/**/*.py")):
    n = f.name
    if not (n.startswith("test_") or n.endswith("_test.py")):
        continue
    src = f.read_text()
    tree = ast.parse(src)
    def tests(body, prefix=""):
        for node in body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith("test"):
                yield prefix + node.name, node
            elif isinstance(node, ast.ClassDef) and node.name.startswith("Test"):
                yield from tests(node.body, node.name + ".")
    for name, node in tests(tree.body):
        seg = ast.get_source_segment(src, node) or ""
        if "flask_client." not in seg:
            continue
        key = f"{f.relative_to(root).as_posix()}::{name}"
        rows.append((hashlib.sha256(key.encode()).hexdigest(), key, node.lineno))
rows.sort()
print(len(rows), "candidate tests")
for h, key, line in rows[:20]:
    print(h[:12], key, line)
