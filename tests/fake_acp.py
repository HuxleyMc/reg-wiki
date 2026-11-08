"""A stand-in ACP agent for the tests: newline-delimited JSON-RPC on stdin/stdout.

Environment:
  FAKE_ACP_REPLIES  file of replies, one per session/prompt, separated by lines of `=====`
  FAKE_ACP_LOG      file to append every message received to, one JSON object per line
  FAKE_ACP_MODE     `hang`: never answer a prompt; `exit`: exit on a prompt;
                    `permission`: ask for permission before replying
"""

import json
import os
import sys
import time

replies = []
path = os.environ.get("FAKE_ACP_REPLIES")
if path:
    with open(path) as f:
        replies = f.read().split("\n=====\n")
mode = os.environ.get("FAKE_ACP_MODE", "")
log_path = os.environ.get("FAKE_ACP_LOG")
model = "default"
next_id = 1000


def log(msg):
    if log_path:
        with open(log_path, "a") as f:
            f.write(json.dumps(msg) + "\n")


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def read():
    line = sys.stdin.readline()
    if not line:
        sys.exit(0)
    msg = json.loads(line)
    log(msg)
    return msg


def config_options():
    return [
        {"id": "mode", "name": "Mode", "category": "mode", "type": "select",
         "currentValue": "default", "options": [{"value": "default", "name": "Default"}]},
        {"id": "model", "name": "Model", "category": "model", "type": "select",
         "currentValue": model,
         "options": [
             {"value": "default", "name": "Default (recommended)"},
             {"value": "claude-haiku-5-5", "name": "Haiku", "description": "Fastest"},
             {"value": "claude-sonnet-5-5", "name": "Sonnet"},
         ]},
    ]


print("fake agent starting up", file=sys.stderr)
sys.stdout.write("this line is not JSON\n")
sys.stdout.flush()

while True:
    msg = read()
    method = msg.get("method")
    mid = msg.get("id")
    params = msg.get("params", {})
    if method == "initialize":
        log({"env": {"REG_LIBRARIAN": os.environ.get("REG_LIBRARIAN"),
                     "CLAUDECODE": os.environ.get("CLAUDECODE")}})
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}})
    elif method == "session/new":
        send({"jsonrpc": "2.0", "id": mid, "result": {
            "sessionId": "s1", "configOptions": config_options()}})
    elif method == "session/set_config_option":
        model = params["value"]
        send({"jsonrpc": "2.0", "id": mid, "result": {"configOptions": config_options()}})
    elif method == "session/prompt":
        if mode == "hang":
            while True:
                m = read()
                if m.get("method") == "session/cancel":
                    time.sleep(30)
        if mode == "exit":
            sys.exit(3)
        if mode == "permission":
            next_id += 1
            send({"jsonrpc": "2.0", "id": next_id, "method": "session/request_permission",
                  "params": {"sessionId": "s1", "toolCall": {"toolCallId": "t1"},
                             "options": [
                                 {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                                 {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}})
            read()  # the answer, logged
            next_id += 1
            send({"jsonrpc": "2.0", "id": next_id, "method": "fs/read_text_file",
                  "params": {"sessionId": "s1", "path": "/etc/passwd"}})
            read()
        text = replies.pop(0) if replies else "no more replies"
        half = len(text) // 2
        for part in (text[:half], text[half:]):
            send({"jsonrpc": "2.0", "method": "session/update", "params": {
                "sessionId": "s1",
                "update": {"sessionUpdate": "agent_message_chunk",
                           "content": {"type": "text", "text": part}}}})
        send({"jsonrpc": "2.0", "id": mid, "result": {"stopReason": "end_turn"}})
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "nope"}})
