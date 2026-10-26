#!/usr/bin/env python3
"""Generates the NCP's EZSP frame dispatcher, emAfProcessEzspCommand(), which
AppBuilder would. The NCP core handles a few frames itself and passes the
rest here.

Every frame is the mirror of its host-side function in the SDK's generated
app/util/ezsp/command-functions.h: what the host appends the NCP fetches,
what the host fetches the NCP appends. The handler is the plugin's mapped
stack function (setup(ezsp) in app/ncp/plugin/plugin.info) or, failing that,
the emberAfEzsp<Frame>CommandCallback a library defines. Frames with neither
are left out: the core answers them as an invalid frame ID.

Usage: gen-dispatch.py ZIGBEE_DIR LIBRARY.a... > dispatch.c
"""
import re
import subprocess
import sys

# The EZSP plugins built in (the Makefile's ncp-*-library), whose frame maps apply.
PLUGINS = ["ezsp-zigbee-pro", "ezsp-security", "ezsp-binding"]

zb, libs = sys.argv[1], sys.argv[2:]


def frame_map():
    """Frame name -> stack function, from the plugins' setup(ezsp) blocks."""
    text = open(f"{zb}/app/ncp/plugin/plugin.info").read()
    out = {}
    for plugin in PLUGINS:
        body = re.search(r"^plugin\(%s\) \{(.*?)^\}" % re.escape(plugin), text, re.S | re.M).group(1)
        block = re.search(r"setup\(ezsp\) \{(.*?)\n  \}", body, re.S).group(1)
        for name, func in re.findall(r"^\s*(\w+)\s*=\s*(\w+)\s*$", block, re.M):
            if name != "sections":
                out[name] = func
    return out


def library_callbacks():
    nm = subprocess.run(["arm-none-eabi-nm", "-g", "--defined-only", *libs],
                        capture_output=True, text=True, check=True).stdout
    return set(re.findall(r" T (emberAfEzsp\w+CommandCallback)$", nm, re.M))


class Param:
    def __init__(self, decl):
        decl = decl.replace("const ", "").strip()
        m = re.match(r"(.*?)(\*?)\s*(\w+)$", decl)
        self.type, self.pointer, self.name = m.group(1).strip(), bool(m.group(2)), m.group(3)
        self.array = self.type == "EmberEUI64"


SCALAR = {"Int8u": "uint8_t", "Int16u": "uint16_t", "Int32u": "uint32_t"}


def mirror(name, ret, params, body):
    """The case body for one frame, or None if a statement has no mirror."""
    params = {p.name: p for p in params}
    decls, fetches, appends = [], [], []
    args = {}
    sent, answer = body.split("sendCommand();")
    for stmt in re.findall(r"^\s*(.*?;)\s*$", sent, re.M):
        # The host's own locals (the returned value) and frame plumbing.
        if stmt.startswith(("startCommand", "EzspStatus")) or re.match(r"\w+ \w+;$", stmt):
            continue
        m = re.match(r"append(Int8u|Int16u|Int32u)\((\w+)\);", stmt)
        if m:
            p = params[m.group(2)]
            fetches.append(f"{p.type} {p.name} = ({p.type})fetch{m.group(1)}();")
            args[p.name] = p.name
            continue
        m = re.match(r"appendInt8uArray\((\w+), (\w+)\);", stmt)
        if m:
            fetches.append(f"uint8_t *{m.group(2)} = fetchInt8uPointer({m.group(1)});")
            args[m.group(2)] = m.group(2)
            continue
        m = re.match(r"appendInt16uArray\((\w+), (\w+)\);", stmt)
        if m:
            fetches.append(f"uint16_t {m.group(2)}[{m.group(1)}];")
            fetches.append(f"fetchInt16uArray({m.group(1)}, {m.group(2)});")
            args[m.group(2)] = m.group(2)
            continue
        m = re.match(r"append(Ember\w+)\((\w+)\);", stmt)
        if m:
            p = params[m.group(2)]
            fetches.append(f"{p.type} {p.name};")
            fetches.append(f"fetch{m.group(1)}(&{p.name});")
            args[p.name] = "&" + p.name
            continue
        return None
    result = None
    for stmt in re.findall(r"^\s*(.*?;)\s*$", answer, re.M):
        if stmt.startswith("EZSP_ASH_TRACE"):
            continue
        m = re.match(r"return (\w+);", stmt)
        if m:
            # The first return hands back the answer; later ones are failures.
            result = result or m.group(1)
            continue
        m = re.match(r"(\*?)(\w+) = fetch(Int8u|Int16u|Int32u)\(\);", stmt)
        if m:
            star, var, kind = m.groups()
            if star:
                p = params[var]
                decls.append(f"{p.type} {var};")
                args[var] = "&" + var
            appends.append(f"append{kind}({var});")
            continue
        m = re.match(r"fetchInt8uArray\((\d+), (\w+)\);", stmt)
        if m:
            decls.append(f"uint8_t {m.group(2)}[{m.group(1)}];")
            args[m.group(2)] = m.group(2)
            appends.append(f"appendInt8uArray({m.group(1)}, {m.group(2)});")
            continue
        m = re.match(r"fetch(Ember\w+)\((\w+)\);", stmt)
        if m:
            p = params[m.group(2)]
            decls.append(f"{p.type} {p.name};")
            args[p.name] = "&" + p.name
            appends.append(f"append{m.group(1)}(&{p.name});")
            continue
        return None
    if any(p not in args for p in params):
        return None
    call = "%s(%s);" % (name, ", ".join(args[p] for p in params))
    if ret != "void":
        call = f"{ret} {result} = {call}"
        # The returned value was declared by the host; drop our own declaration.
        decls = [d for d in decls if not d.endswith(f" {result};")]
    return fetches + decls + [call] + appends


def main():
    frames = frame_map()
    callbacks = library_callbacks()
    text = open(f"{zb}/app/util/ezsp/command-functions.h").read()
    cases, protos, skipped = [], [], []
    for m in re.finditer(r"^(\w[\w ]*?\**) ?ezsp(\w+)\(([^)]*)\)\n\{(.*?)\n\}", text, re.S | re.M):
        ret, frame, plist, body = m.group(1).strip(), m.group(2), m.group(3), m.group(4)
        cmd = re.search(r"startCommand\((EZSP_\w+)\)", body)
        if not cmd or "sendCommand();" not in body:
            continue
        name = frame[0].lower() + frame[1:]
        params = [Param(p) for p in plist.split(",") if p.strip() not in ("", "void")]
        callback = f"emberAfEzsp{frame}CommandCallback"
        if name in frames:
            handler = frames[name]
            if handler.startswith("emberAf"):  # a plugin's, with no header
                protos.append("%s %s(%s);" % (ret, handler, plist.strip() or "void"))
        elif callback in callbacks:
            handler = callback
            protos.append("%s %s(%s);" % (ret, callback, plist.strip() or "void"))
        else:
            continue
        lines = mirror(handler, ret, params, body)
        if lines is None:
            skipped.append(name)
            continue
        cases.append((cmd.group(1), lines))
    if skipped:
        print("gen-dispatch: no mirror for " + ", ".join(skipped), file=sys.stderr)

    print("// Generated by gen-dispatch.py from the SDK's command-functions.h. Do not edit.")
    print('#include PLATFORM_HEADER')
    print('#include "stack/include/ember.h"')
    print('#include "app/util/ezsp/ezsp-protocol.h"')
    print('#include "app/util/ezsp/ezsp-frame-utilities.h"')
    print()
    for p in protos:
        print(p)
    print()
    print("bool emAfProcessEzspCommand(uint16_t commandId)")
    print("{")
    print("  switch (commandId) {")
    for cmd, lines in cases:
        print(f"    case {cmd}: {{")
        for line in lines:
            print("      " + line)
        print("      return true;")
        print("    }")
    print("    default:")
    print("      return false;")
    print("  }")
    print("}")


main()
