#!/usr/bin/env python3
"""Generate src/eval/reflect_base.txt, the declarations of the standard
library's `pkl:base` classes that `pkl:reflect` describes.

Usage: scripts/gen-reflect-base.py PATH/TO/stdlib/base.pkl > src/eval/reflect_base.txt

The input is Apple's base.pkl at the tag matching the pkl version pklr follows
(see `PKL_VERSION` in src/eval/reflect_base.rs). The output keeps what
reflection reports, one record per line, so pklr need not embed or parse the
whole module. A field never holds a tab or a newline. Records:

  C line modifiers name type-parameters extends   a class
  P line modifiers name                           a property of the last class
  M line modifiers name type-parameters params    a method of the last class
  D text                                          a line of the preceding
                                                  record's doc comment
  A name field...                                 an annotation of the
                                                  preceding record

An annotation field is `key=value` for a string property and `key[]=a<US>b`
for a listing of strings, where <US> is the unit separator (U+001F).

Modifiers are comma-separated, type parameters are `variance:name` pairs
separated by commas (the variance may be empty), and params are names
separated by commas.
"""
import re
import sys

MODS = ("abstract", "external", "open", "local", "hidden", "fixed", "const")
CLASS = re.compile(r"^((?:(?:%s) )*)class (\w+)" % "|".join(MODS))
MEMBER = re.compile(r"^  ((?:(?:%s) )*)(function )?(\w+)" % "|".join(MODS))


def split_top(text):
    """Split on commas that are not nested in brackets or parens."""
    parts, depth, cur = [], 0, ""
    for index, ch in enumerate(text):
        if ch in "<([{":
            depth += 1
        elif ch in ">)]}" and not (ch == ">" and text[index - 1 : index] == "-"):
            # The `>` of a function type's `->` closes nothing.
            depth -= 1
        if ch == "," and depth == 0:
            parts.append(cur.strip())
            cur = ""
        else:
            cur += ch
    if cur.strip():
        parts.append(cur.strip())
    return parts


def matching(text, start):
    """The index of the bracket that closes the one at `text[start]`, not
    counting the `>` of a `->`."""
    depth = 0
    for index in range(start, len(text)):
        ch = text[index]
        if ch in "<([{":
            depth += 1
        elif ch in ">)]}" and not (ch == ">" and text[index - 1] == "-"):
            depth -= 1
            if depth == 0:
                return index
    raise AssertionError(text)


def signature(text):
    """The type parameters and parameters of the method declared by `text`:
    `function name<T>(a: A, b: (X) -> Y): R`."""
    m = re.match(r"^\s*(?:(?:%s) )*function (\w+)" % "|".join(MODS), text)
    assert m, text
    pos = m.end()
    tparams = ""
    if text[pos] == "<":
        end = matching(text, pos)
        tparams = text[pos + 1 : end]
        pos = end + 1
    assert text[pos] == "(", text
    end = matching(text, pos)
    return tparams, text[pos + 1 : end]


def type_params(text):
    out = []
    for part in split_top(text):
        words = part.split()
        variance = words[0] if len(words) == 2 else ""
        out.append(f"{variance}:{words[-1]}")
    return ",".join(out)


def balanced(text):
    return all(text.count(a) == text.count(b) for a, b in ("()", "{}", "[]"))


def annotation_fields(body):
    """The `key=value` fields of an annotation body such as
    `{ names { "a"; "b" } }` or `{ since = "1"; message = "m" }`."""
    inner = body.strip()[1:-1].strip()
    fields = []
    for match in re.finditer(
        r'(\w+)\s*(?:=\s*"((?:[^"\\]|\\.)*)"|\{((?:\s*"(?:[^"\\]|\\.)*"\s*;?)*)\s*\})', inner
    ):
        key, string, items = match.groups()
        if string is not None:
            fields.append(f"{key}={string}")
        else:
            values = re.findall(r'"((?:[^"\\]|\\.)*)"', items)
            fields.append(f"{key}[]=" + "\x1f".join(values))
    assert fields, body
    return fields


def main(path):
    lines = open(path).read().split("\n")
    out = []
    doc, annotations = [], []
    in_class = False
    i = 0

    def emit(record, fields):
        for field in fields:
            assert "\t" not in field and "\n" not in field, field
        out.append("\t".join([record] + list(fields)))
        for text in doc:
            out.append("D\t" + text)
        for name, body in annotations:
            out.append("\t".join(["A", name] + annotation_fields(body)))

    def gather(start):
        """The logical declaration beginning at line `start`."""
        j, text = start, lines[start]
        return j, text

    while i < len(lines):
        line = lines[i]
        if line.startswith("///") or line.startswith("  ///"):
            # Pkl keeps a doc line's trailing whitespace.
            text = line.lstrip().rstrip("\r\n")[3:]
            doc.append(text[1:] if text.startswith(" ") else text)
            i += 1
            continue
        stripped = line.strip()
        if stripped.startswith("@"):
            text, j = line.strip(), i
            while not balanced(text):
                j += 1
                text += " " + lines[j].strip()
            m = re.match(r"@(\w+)\s*(\{.*\})?$", text)
            assert m, text
            annotations.append((m.group(1), m.group(2) or "{}"))
            i = j + 1
            continue
        m = CLASS.match(line)
        if m:
            j, head = i, line
            # The header runs to the `{` that opens the body, if any.
            if not head.rstrip().endswith("{") and lines[i + 1].startswith("  extends"):
                j += 1
                head += " " + lines[j].strip()
            hm = re.match(
                r"^((?:(?:%s) )*)class (\w+)(<.*?>)?(?: extends (.*?))?\s*\{?\s*$" % "|".join(MODS),
                head,
            )
            assert hm, head
            mods = [x for x in hm.group(1).split()]
            emit(
                "C",
                [
                    str(i + 1),
                    ",".join(mods),
                    hm.group(2),
                    type_params(hm.group(3)[1:-1]) if hm.group(3) else "",
                    hm.group(4) or "",
                ],
            )
            in_class = head.rstrip().endswith("{")
            doc, annotations = [], []
            i = j + 1
            continue
        if in_class and line == "}":
            in_class = False
        mm = MEMBER.match(line) if in_class else None
        if mm and not stripped.startswith("///"):
            # pkl reports `external` only on classes.
            mods = [m for m in mm.group(1).split() if m != "external"]
            if "local" in mm.group(1).split():
                doc, annotations = [], []
                i += 1
                continue
            name = mm.group(3)
            if mm.group(2):
                j, text = i, line
                while not balanced(text):
                    j += 1
                    text += " " + lines[j].strip()
                tparams, plist = signature(text)
                params = [p.split(":")[0].strip() for p in split_top(plist)]
                emit(
                    "M",
                    [
                        str(i + 1),
                        ",".join(mods),
                        name,
                        type_params(tparams) if tparams else "",
                        ",".join(params),
                    ],
                )
                i = j + 1
            else:
                emit("P", [str(i + 1), ",".join(mods), name])
                i += 1
            doc, annotations = [], []
            continue
        if stripped == "" or stripped.startswith("//"):
            i += 1
            continue
        # Anything else ends a pending doc comment and annotations.
        doc, annotations = [], []
        i += 1
    sys.stdout.write("\n".join(out) + "\n")


main(sys.argv[1])
