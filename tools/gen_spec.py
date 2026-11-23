#!/usr/bin/env python3
"""Ringkas elasticsearch-specification (schema.json) menjadi spec autocomplete kecil.

Sumber: https://github.com/elastic/elasticsearch-specification (Apache-2.0),
file output/schema/schema.json pada branch versi (mis. 8.19, 7.17).

Pemakaian:
    python3 tools/gen_spec.py schema-8.19.json src/es/spec/es8.json

Format output:
    {"endpoints": [{"n": name, "doc": url, "u": [[methods, path]], "q": {param: ref}, "b": ref|null}],
     "types": {Name: {"p": {prop: ref}}}}

ref:
    "s" string, "n" number, "b" boolean, "f" field name, "i" index name, "x" apa saja
    {"e": [..]} enum, {"a": ref} array, {"d": ref, "k": "f"|"s"} dictionary, {"u": [ref]} union,
    {"t": Name} objek di "types"
"""

import json
import sys

PRIMITIVES = {
    "string": "s", "number": "n", "integer": "n", "long": "n", "float": "n", "double": "n",
    "short": "n", "byte": "n", "uint": "n", "ulong": "n", "boolean": "b", "null": "x",
    "binary": "x", "void": "x", "UserDefinedValue": "x",
}
SPECIAL = {
    "Field": "f", "Fields": "f", "IndexName": "i", "Indices": "i", "IndexAlias": "i",
    "DataStreamName": "i", "DataStreamNames": "i",
}


def main(src, dst):
    schema = json.load(open(src))
    types = {(t["name"]["namespace"], t["name"]["name"]): t for t in schema["types"]}
    out_types = {}
    alias_cache = {}

    def key_of(name):
        return f'{name["name"]}@{name["namespace"]}'

    def ref(v, depth=0):
        kind = v["kind"]
        if kind == "instance_of":
            return named(v["type"], depth)
        if kind == "array_of":
            return {"a": ref(v["value"], depth)}
        if kind == "dictionary_of":
            k = ref(v["key"], depth)
            return {"d": ref(v["value"], depth), "k": "f" if k == "f" else "s"}
        if kind == "union_of":
            items = []
            for item in v["items"]:
                r = ref(item, depth)
                if r not in items:
                    items.append(r)
            return items[0] if len(items) == 1 else {"u": items}
        if kind == "literal_value":
            return {"e": [v["value"]]}
        return "x"

    def named(name, depth):
        n = name["name"]
        if n in SPECIAL:
            return SPECIAL[n]
        if name["namespace"] == "_builtins" or n in PRIMITIVES:
            return PRIMITIVES.get(n, "x")
        t = types.get((name["namespace"], n))
        if t is None:  # parameter generic
            return "x"
        kind = t["kind"]
        if kind == "enum":
            return {"e": [m["name"] for m in t["members"]]}
        if kind == "type_alias":
            k = key_of(name)
            if k in alias_cache:
                return alias_cache[k]
            alias_cache[k] = "x"  # cegah rekursi
            r = ref(t["type"], depth + 1) if depth < 50 else "x"
            alias_cache[k] = r
            return r
        if kind == "interface":
            k = key_of(name)
            if k not in out_types:
                out_types[k] = None  # tandai sedang diproses
                out_types[k] = {"p": interface_props(t)}
            return {"t": k}
        return "x"

    def interface_props(t):
        props = {}
        parent = t.get("inherits")
        while parent:
            pt = types.get((parent["type"]["namespace"], parent["type"]["name"]))
            if not pt:
                break
            for p in pt.get("properties", []):
                props.setdefault(p["name"], p)
            parent = pt.get("inherits")
        for p in t.get("properties", []):
            props[p["name"]] = p
        out = {}
        for name, p in props.items():
            r = ref(p["type"])
            out[name] = r
            for alias in p.get("aliases", []):
                out[alias] = r
        return out

    endpoints = []
    for e in schema["endpoints"]:
        if e["name"].startswith("_internal"):
            continue
        req = types.get((e["request"]["namespace"], e["request"]["name"])) if e.get("request") else None
        query, body = {}, None
        if req:
            for p in req.get("query", []):
                query[p["name"]] = ref(p["type"])
            b = req.get("body", {})
            if b.get("kind") == "properties":
                body = {"p": {}}
                for p in b["properties"]:
                    r = ref(p["type"])
                    body["p"][p["name"]] = r
                    for alias in p.get("aliases", []):
                        body["p"][alias] = r
            elif b.get("kind") == "value":
                body = ref(b["value"])
        urls = [[" ".join(u["methods"]), u["path"]] for u in e["urls"]]
        doc = (e.get("docUrl") or "").replace("{branch}", "current")
        endpoints.append({"n": e["name"], "doc": doc, "u": urls, "q": query, "b": body})

    json.dump({"endpoints": endpoints, "types": out_types}, open(dst, "w"), separators=(",", ":"))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
