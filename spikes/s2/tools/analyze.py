"""Measure a Charon output file (JSON, written with --no-dedup-serialized-ast).

Usage: python3 -I analyze.py <file.llbc> [--dump NAME]

It reports, for the local functions only:
- the number of function declarations by kind and by body kind;
- the closures and the async functions;
- a census of the LLBC statement kinds;
- the code duplication (statement IDs that occur more than once in a body);
- the span check (statement spans inside the function span, in a local file).
"""
import collections
import json
import sys


def key(value):
    return value if isinstance(value, str) else next(iter(value))


def name_of(item, impl_names):
    parts = []
    for elem in item["item_meta"]["name"]:
        kind = key(elem)
        if kind == "Ident":
            parts.append(elem["Ident"][0])
        elif kind == "Impl":
            ref = elem["Impl"]
            parts.append(impl_names.get(json.dumps(ref), "{impl}"))
        else:
            parts.append("{" + kind + ":" + json.dumps(elem[kind])[:20] + "}")
    return "::".join(parts)


def span_data(span):
    if span is None:
        return None, None
    if "Untagged" in span:
        return span["Untagged"]["data"], span["Untagged"].get("generated_from_span")
    if "Value" in span:  # dedup format, not expected here
        return span_data(span["Value"][1])
    return span.get("data"), span.get("generated_from_span")


def main():
    path = sys.argv[1]
    dump = sys.argv[3] if len(sys.argv) > 3 and sys.argv[2] == "--dump" else None
    root = json.load(open(path))
    crate = root["translated"]
    files = crate["files"]
    # Charon tags std and registry paths as `Local` too; a crate file has a relative path.
    local_files = {i for i, f in enumerate(files) if f and key(f["name"]) == "Local" and not f["name"]["Local"].startswith("/")}

    closure_impls = {}
    impl_names = {}
    for impl in crate["trait_impls"]:
        if not impl:
            continue
        src = impl.get("src")
        if isinstance(src, dict) and "Closure" in src:
            closure_impls[impl["def_id"]] = src["Closure"]["kind"]
            impl_names[json.dumps({"Trait": impl["def_id"]})] = "{closure " + src["Closure"]["kind"] + "}"

    report = collections.OrderedDict()
    decls = [f for f in crate["fun_decls"] if f and f["item_meta"]["is_local"]]
    report["local_fun_decls"] = len(decls)
    body_kinds = collections.Counter()
    src_kinds = collections.Counter()
    closures = collections.defaultdict(list)  # closure span -> [(method, body kind)]
    errors = []
    census = collections.Counter()
    switch_kinds = collections.Counter()
    dup_bodies = 0
    dup_statements = 0
    total_statements = 0
    span_in = span_out = span_foreign = span_generated = 0
    with_unwind = 0
    region_spans = collections.Counter()

    for decl in decls:
        body = decl["body"]
        body_kind = key(body)
        body_kinds[body_kind] += 1
        src = decl.get("src")
        src_kind = key(src) if src is not None else "None"
        name_parts = decl["item_meta"]["name"]
        closure_kind = None
        for elem in name_parts:
            if key(elem) == "Impl" and "Trait" in elem["Impl"] and elem["Impl"]["Trait"] in closure_impls:
                closure_kind = closure_impls[elem["Impl"]["Trait"]]
        builtin = any(key(e) == "Builtin" for e in name_parts)
        if builtin:
            src_kind = "builtin(drop glue)"
        elif closure_kind:
            src_kind = "closure method"
            data, _ = span_data(decl["item_meta"]["span"])
            closures[json.dumps(data)].append((name_parts[-1].get("Ident", ["?"])[0], body_kind))
        src_kinds[src_kind] += 1
        name = name_of(decl, impl_names)
        if body_kind == "Error" or decl["item_meta"].get("has_errors"):
            errors.append((name, body_kind, str(body.get("Error", ""))[:160] if isinstance(body, dict) else ""))
        if body_kind not in ("Structured", "Unstructured"):
            continue
        expr = body[body_kind]
        fdata, _ = span_data(decl["item_meta"]["span"])
        seen = collections.Counter()

        def visit_statement(stmt):
            nonlocal span_in, span_out, span_foreign, span_generated, with_unwind
            kind = key(stmt["kind"])
            census[kind] += 1
            if kind == "Switch":
                scrutinee = stmt["kind"]["Switch"]["data"]["scrutinee"]
                switch_kinds[key(scrutinee)] += 1
            if kind == "Call" and isinstance(stmt["kind"]["Call"], dict) and stmt["kind"]["Call"].get("on_unwind"):
                with_unwind += 1
            data, generated = span_data(stmt["span"])
            if kind in ("Assign", "Call"):
                seen[json.dumps([data, stmt["kind"]])] += 1
            if kind in ("Loop", "Switch"):
                good = data and fdata and data["file_id"] == fdata["file_id"] and data["beg"]["line"] >= fdata["beg"]["line"]
                region_spans["ok" if good else "bad_start"] += 1
            if generated:
                span_generated += 1
            if data is None or data["file_id"] not in local_files:
                span_foreign += 1
            elif fdata and fdata["file_id"] == data["file_id"] and fdata["beg"]["line"] <= data["beg"]["line"] and data["end"]["line"] <= fdata["end"]["line"]:
                span_in += 1
            else:
                span_out += 1

        def walk(node):
            if isinstance(node, dict):
                if "statements" in node and isinstance(node["statements"], list):
                    for stmt in node["statements"]:
                        visit_statement(stmt)
                for value in node.values():
                    walk(value)
            elif isinstance(node, list):
                for value in node:
                    walk(value)

        if body_kind == "Structured":
            walk(expr["body"])
            dups = sum(n - 1 for n in seen.values() if n > 1)
            total_statements += sum(seen.values())  # Assign and Call only
            dup_statements += dups
            dup_bodies += 1 if dups else 0
        else:
            for block in expr["body"]:
                for stmt in block["statements"]:
                    visit_statement(stmt)
                term = block["terminator"]
                census["T:" + key(term["kind"])] += 1
        if dump and dump in name:
            print("DUMP", name, json.dumps(expr["body"])[:400])

    report["body_kinds"] = dict(body_kinds)
    report["decl_kinds"] = dict(src_kinds)
    # Each closure has one FnOnce impl (and FnMut / Fn impls if it allows them).
    report["closure_impls"] = dict(collections.Counter(closure_impls.values()))
    report["closure_spans"] = len(closures)
    report["closure_spans_all_bodies_ok"] = sum(all(b == "Structured" or b == "Unstructured" for _, b in v) for v in closures.values())
    report["errors"] = errors
    report["census"] = dict(census.most_common())
    report["switch_kinds"] = dict(switch_kinds)
    report["calls_with_unwind_block"] = with_unwind
    if total_statements:
        report["duplication"] = {"bodies_with_dup": dup_bodies, "dup_statements": dup_statements, "statements": total_statements}
    report["region_header_spans"] = dict(region_spans)
    report["spans"] = {"inside_fn": span_in, "outside_fn_same_crate": span_out, "foreign_file": span_foreign, "macro_generated": span_generated}
    print(json.dumps(report, indent=1))


main()
