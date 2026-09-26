# Python hash comments and module, class and function docstrings.

import ast
import io
import json
import sys
import tokenize

if sys.version_info < (3, 12):
    raise RuntimeError("comment extraction requires Python 3.12 or newer")

source = sys.stdin.read()
data = source.encode("utf-8")
lines = data.splitlines(keepends=True)
offsets = [0]
for line in lines:
    offsets.append(offsets[-1] + len(line))


def token_offset(position):
    row, column = position
    return offsets[row - 1] + len(lines[row - 1].decode("utf-8")[:column].encode("utf-8"))


found = []
for token in tokenize.tokenize(io.BytesIO(data).readline):
    if token.type == tokenize.COMMENT:
        if token.start == (1, 0) and token.string.startswith("#!/"):
            continue
        found.append([token_offset(token.start), token_offset(token.end), False])

for node in ast.walk(ast.parse(source)):
    if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)) and node.body:
        expr = node.body[0]
        if isinstance(expr, ast.Expr) and isinstance(expr.value, ast.Constant) and isinstance(expr.value.value, str):
            value = expr.value
            found.append([
                offsets[value.lineno - 1] + value.col_offset,
                offsets[value.end_lineno - 1] + value.end_col_offset,
                True,
            ])

json.dump(found, sys.stdout)
