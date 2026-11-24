"""Derive timing-enabled and timing-erased workers from the same product source."""
import ast
import hashlib
import json
from pathlib import Path
import sys


def observer(node):
    return any((isinstance(value, ast.Name) and value.id == "update_timing")
               or (isinstance(value, ast.Attribute) and value.attr == "timings")
               for value in ast.walk(node))


class EraseTiming(ast.NodeTransformer):
    def visit_Import(self, node):
        node.names = [name for name in node.names if name.name != "lifecycle_timing"]
        return node if node.names else None

    def visit_Assign(self, node):
        return None if any(observer(target) for target in node.targets) else self.generic_visit(node)

    def visit_Expr(self, node):
        return None if observer(node) else self.generic_visit(node)

    def visit_If(self, node):
        return None if observer(node.test) else self.generic_visit(node)

    def visit_With(self, node):
        if any(observer(item.context_expr) for item in node.items):
            return [result for statement in node.body
                    for result in self._statements(self.visit(statement))]
        return self.generic_visit(node)

    @staticmethod
    def _statements(value):
        return value if isinstance(value, list) else ([] if value is None else [value])

    def visit_FunctionDef(self, node):
        node.decorator_list = [value for value in node.decorator_list if not observer(value)]
        defaults = dict(zip([arg.arg for arg in node.args.args[-len(node.args.defaults):]], node.args.defaults)) if node.args.defaults else {}
        node.args.args = [arg for arg in node.args.args if arg.arg != "trace_role"]
        node.args.defaults = [defaults[arg.arg] for arg in node.args.args if arg.arg in defaults]
        return self.generic_visit(node)

    def visit_Call(self, node):
        node.keywords = [keyword for keyword in node.keywords if keyword.arg != "trace_role"]
        return self.generic_visit(node)


class EraseStackCapture(ast.NodeTransformer):
    def visit_Import(self, node):
        node.names = [name for name in node.names if name.name != "traceback"]
        return node if node.names else None

    def visit_Expr(self, node):
        if (isinstance(node.value, ast.Call) and isinstance(node.value.func, ast.Name)
                and node.value.func.id == "print"
                and any(isinstance(value, ast.Name) and value.id == "traceback" for value in ast.walk(node))):
            return None
        return self.generic_visit(node)

    def visit_Call(self, node):
        node.keywords = [keyword for keyword in node.keywords if keyword.arg != "traceback"]
        return self.generic_visit(node)


def generate(source, enabled, minimal=False):
    semantic = EraseStackCapture().visit(EraseTiming().visit(ast.parse(source)))
    ast.fix_missing_locations(semantic)
    digest = hashlib.sha256(ast.dump(semantic, include_attributes=False).encode()).hexdigest()
    selected = ast.parse(source) if enabled else EraseTiming().visit(ast.parse(source))
    if minimal:
        selected = EraseStackCapture().visit(selected)
    ast.fix_missing_locations(selected)
    output = source if enabled and not minimal else ast.unparse(selected) + "\n"
    compile(output, "<lifecycle-postimage>", "exec")
    return output, digest


def main():
    root, output, enabled = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3] == "observable"
    output.mkdir(parents=True, exist_ok=True)
    manifest = {}
    features = json.loads(sys.argv[4]) if len(sys.argv) > 4 else []
    for name in ("window.py", "external.py", "runner.py"):
        source = (root / "src/lifecycle" / name).read_text()
        generated, semantic = generate(source, enabled, minimal="observability" not in features)
        (output / name).write_text(generated)
        manifest[name] = dict(source_sha256=hashlib.sha256(source.encode()).hexdigest(),
                              semantic_sha256=semantic, postimage_sha256=hashlib.sha256(generated.encode()).hexdigest())
    (output / "manifest.json").write_text(json.dumps(dict(schema_version=1, timings=enabled, features=features, files=manifest), indent=2) + "\n")


if __name__ == "__main__":
    main()
