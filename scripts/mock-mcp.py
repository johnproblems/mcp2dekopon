#!/usr/bin/env python3
"""Loopback MCP mock for mcp2dekopon end-to-end validation (stdlib only).

Serves five tools over Streamable HTTP: the first tools/list page is plain
JSON with a cursor, the second page arrives as SSE. Mirrors the Rust mock in
tests/gen_mock.rs; keep the tool shapes in sync.
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SESSION = "mock-session-1"
PROTOCOL = "2025-06-18"

SEARCH = {
    "name": "search",
    "description": "Search the mock index.",
    "annotations": {"readOnlyHint": True},
    "inputSchema": {
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "The question."},
            "limit": {"type": "integer"},
        },
        "required": ["query"],
    },
}

DETAILS = {
    "name": "get_details",
    "description": "Fetch one record with options.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "key": {"type": "string"},
            "options": {
                "type": "object",
                "properties": {
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "labels": {
                        "type": "object",
                        "additionalProperties": {"type": "string"},
                    },
                },
                "required": ["tags"],
            },
        },
        "required": ["key"],
    },
}

LOCATE = {
    "name": "locate",
    "description": "Locate a repo or a gist.",
    "annotations": {"destructiveHint": True},
    "inputSchema": {
        "type": "object",
        "properties": {
            "target": {
                "oneOf": [
                    {
                        "title": "Repo",
                        "type": "object",
                        "properties": {"repo": {"type": "string"}},
                        "required": ["repo"],
                    },
                    {
                        "title": "Gist",
                        "type": "object",
                        "properties": {"gist": {"type": "string"}},
                    },
                ],
            },
        },
        "required": ["target"],
    },
}

RECURSE = {
    "name": "recurse",
    "description": "Never inlines.",
    "inputSchema": {
        "type": "object",
        "$defs": {
            "Node": {
                "type": "object",
                "properties": {"child": {"$ref": "#/$defs/Node"}},
            },
        },
        "properties": {"node": {"$ref": "#/$defs/Node"}},
    },
}

EITHER = {
    "name": "either",
    "description": "Takes a value or a list of values.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "value": {"oneOf": [{"type": "string"}, {"type": "integer"}]},
        },
    },
}

REMAP = {
    "name": "remap",
    "description": "Takes free-form arguments.",
    "inputSchema": {
        "type": "object",
        "additionalProperties": True,
    },
}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def _send(self, status, body, content_type="application/json", extra=()):
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        for key, value in extra:
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        try:
            body = json.loads(self.rfile.read(length) or b"null")
        except json.JSONDecodeError:
            body = {}
        method = body.get("method", "")
        rpc_id = body.get("id")

        def envelope(result=None, error=None):
            payload = {"jsonrpc": "2.0", "id": rpc_id}
            if error is not None:
                payload["error"] = error
            else:
                payload["result"] = result
            return json.dumps(payload)

        if method == "initialize":
            self._send(
                200,
                envelope(
                    {
                        "protocolVersion": PROTOCOL,
                        "capabilities": {},
                        "serverInfo": {"name": "mock-mcp", "version": "0.0.0"},
                    }
                ),
                extra=[("Mcp-Session-Id", SESSION)],
            )
        elif method == "notifications/initialized":
            self._send(202, "")
        elif method == "tools/list":
            if REQUIRE_AUTH and self.headers.get("Authorization") != REQUIRE_AUTH:
                self._send(
                    401,
                    envelope(
                        error={"code": -32001, "message": "missing authorization"}
                    ),
                )
                return
            tools = list(EXTRA_TOOLS)
            cursor = (body.get("params") or {}).get("cursor")
            if cursor is None:
                self._send(
                    200,
                    envelope(
                        {
                            "tools": [SEARCH, DETAILS] + tools[:2],
                            "nextCursor": "page2",
                        },
                    ),
                )
            elif cursor == "page2" and len(tools) > 2:
                self._send(
                    200,
                    envelope({"tools": tools[2:], "nextCursor": "page3"}),
                )
            else:
                payload = envelope(
                    {"tools": [LOCATE, RECURSE, EITHER, REMAP]},
                )
                self._send(
                    200,
                    f"event: message\ndata: {payload}\n\n",
                    content_type="text/event-stream",
                )
        elif method == "tools/call":
            self._send(
                200,
                envelope({"content": [{"type": "text", "text": "mock-ok"}]}),
            )
        else:
            self._send(
                200, envelope(error={"code": -32601, "message": "unknown"})
            )


EXTRA_TOOLS = []
REQUIRE_AUTH = ""


def make_extra(index):
    return {
        "name": f"extra_tool_{index:02d}",
        "description": f"Synthetic tool {index} for split testing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": {"type": "string"},
                "count": {"type": "integer"},
            },
            "required": ["text"],
        },
    }


def main():
    global EXTRA_TOOLS, REQUIRE_AUTH
    args = sys.argv[1:]
    port = 0
    extra = 0
    while args:
        arg = args.pop(0)
        if arg == "--extra" and args:
            extra = int(args.pop(0))
        elif arg == "--require-auth" and args:
            REQUIRE_AUTH = args.pop(0)
        else:
            port = int(arg)
    EXTRA_TOOLS = [make_extra(i) for i in range(extra)]
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(server.server_address[1], flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
