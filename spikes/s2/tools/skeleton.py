"""Print the control skeleton of one LLBC function (plain JSON output).

Usage: python3 -I skeleton.py <file.llbc> <name-suffix> [<nth>]

The skeleton keeps loop, switch, break, continue, return, panic, assert, and calls. It drops the
unwind blocks, storage markers, drops, and plain assignments. Each line ends with the source line
range of the statement.
"""
import json
import sys


def key(value):
    return value if isinstance(value, str) else next(iter(value))


def fn_name(decl):
    return "::".join(e["Ident"][0] if "Ident" in e else "{" + key(e) + "}" for e in decl["item_meta"]["name"])


def lines(span):
    data = span["Untagged"]["data"]
    return f"L{data['beg']['line']}-{data['end']['line']}"


def callee(call):
    func = call["call"]["func"]
    text = json.dumps(func)
    # A rough name: the last Ident of a regular fun ref is not in the call; print the id only.
    if "Regular" in func:
        kind = func["Regular"]["kind"]
        if "Fun" in kind:
            return "fun#" + json.dumps(kind["Fun"])[:30]
        if "Trait" in kind:
            return "trait-method#" + str(kind["Trait"][1])
    return text[:30]


def block(node, depth, out):
    for stmt in node["statements"]:
        statement(stmt, depth, out)


def statement(stmt, depth, out):
    pad = "  " * depth
    kind = key(stmt["kind"])
    value = stmt["kind"][kind] if isinstance(stmt["kind"], dict) else None
    where = lines(stmt["span"])
    if kind == "Loop":
        out.append(f"{pad}loop  {where}")
        block(value, depth + 1, out)
    elif kind == "Switch":
        data = value["data"]
        scrutinee = key(data["scrutinee"])
        out.append(f"{pad}switch({scrutinee}) {where}")
        for index, branch in enumerate(value["branches"]):
            out.append(f"{pad}  arm {index}:")
            block(branch, depth + 2, out)
    elif kind in ("Break", "Continue"):
        out.append(f"{pad}{kind.lower()} {value} {where}")
    elif kind in ("Return", "Panic", "Abort", "Assert", "UndefinedBehavior"):
        out.append(f"{pad}{kind.lower()} {where}")
    elif kind == "Call":
        out.append(f"{pad}call {callee(value)} {where}")


def main():
    crate = json.load(open(sys.argv[1]))["translated"]
    suffix = sys.argv[2]
    nth = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    matches = [d for d in crate["fun_decls"] if d and d["item_meta"]["is_local"] and fn_name(d).endswith(suffix)]
    decl = matches[nth]
    print(fn_name(decl), lines(decl["item_meta"]["span"]), "body:", key(decl["body"]), f"({len(matches)} matches)")
    body = decl["body"].get("Structured") if isinstance(decl["body"], dict) else None
    if body is None:
        print(json.dumps(decl["body"])[:300])
        return
    out = []
    block(body["body"], 1, out)
    print("\n".join(out))


main()
