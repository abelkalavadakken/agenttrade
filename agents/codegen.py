# /// script
# requires-python = ">=3.11"
# dependencies = ["grpcio-tools==1.66.2", "protobuf>=5.27,<6"]
# ///
"""Generate the Python side of the contract from proto/agenttrade/v1.

Run: uv run agents/codegen.py
Writes agents/agenttrade_pb/ (gitignored). Rust and Python are generated
from the same file, so a contract change is one proto PR and a rerun here.
"""

from __future__ import annotations

import importlib
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROTO_DIR = ROOT / "proto"
PROTO = PROTO_DIR / "agenttrade" / "v1" / "agenttrade.proto"
OUT = ROOT / "agents" / "agenttrade_pb"


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "__init__.py").write_text(
        '"""Generated from proto/agenttrade/v1. Do not edit; run agents/codegen.py."""\n'
    )
    args = [
        sys.executable,
        "-m",
        "grpc_tools.protoc",
        f"-I{PROTO_DIR}",
        f"--python_out={OUT}",
        f"--pyi_out={OUT}",
        f"--grpc_python_out={OUT}",
        str(PROTO),
    ]
    subprocess.run(args, check=True)
    # protoc lays the package path out as agenttrade/v1/; make it importable.
    pkg = OUT / "agenttrade" / "v1"
    for d in (OUT / "agenttrade", pkg):
        (d / "__init__.py").touch()
    sys.path.insert(0, str(OUT))
    pb2 = importlib.import_module("agenttrade.v1.agenttrade_pb2")
    grpc = importlib.import_module("agenttrade.v1.agenttrade_pb2_grpc")
    names = [n for n in dir(pb2) if n[0].isupper() and not n.startswith("DESCRIPTOR")]
    have = {"SubmitIntentRequest", "GetStateResponse", "StrategyState", "Wake", "StateHash"}
    missing = have - set(names)
    if missing:
        print(f"generated module lacks {sorted(missing)}", file=sys.stderr)
        return 1
    stub = getattr(grpc, "AgentCoreServiceStub", None)
    if stub is None:
        print("generated grpc module lacks AgentCoreServiceStub", file=sys.stderr)
        return 1
    print(f"generated {len(names)} messages and enums into {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
