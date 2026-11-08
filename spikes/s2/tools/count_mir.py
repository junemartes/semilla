"""Count the MIR bodies in a `-Zunpretty=mir` dump, by kind.

A `const fn` has two bodies in the dump (runtime and CTFE). They are adjacent and have the same
header, so the script drops a header that repeats the previous header.
"""
import json
import re
import sys

text = open(sys.argv[1]).read().splitlines()
headers = [line for line in text if re.match(r"^(fn|const|static) ", line)]
bodies = []
previous = None
for header in headers:
    if header == previous:
        continue
    previous = header
    bodies.append(header)

fns = [h for h in bodies if h.startswith("fn ")]
coroutines = [h for h in fns if re.search(r"_1: Pin<&mut \{(async fn body|async block|async closure)", h)]
closures = [h for h in fns if "{closure#" in h.split("(")[0] and h not in coroutines]
# A desugared `async fn` is the outer fn that returns `{async fn body of ...}`.
async_fn_outer = [h for h in fns if re.search(r"-> \{async fn body of", h)]
derived = [h for h in fns if re.search(r"<impl at src/[^>]*>::(fmt|clone|eq|ne|cmp|partial_cmp|hash|default)\(", h)]
macro_impls = [h for h in fns if "<impl at /" in h]
print(json.dumps({
    "bodies": len(bodies),
    "fn_bodies": len(fns),
    "const_static": len(bodies) - len(fns),
    "closures": len(closures),
    "coroutines": len(coroutines),
    "async_fn": len(async_fn_outer),
    "plain_fn": len(fns) - len(closures) - len(coroutines),
    "derive_like": len(derived),
    "from_dep_macros": len(macro_impls),
}))
