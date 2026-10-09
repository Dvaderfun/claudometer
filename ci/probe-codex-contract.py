"""Manual Windows app-server audit: synthetic tokens and a local fake backend only.

This does not validate a production adapter or measure CODEX-02 latency.
Pass the native codex.exe explicitly; no npm shim or real profile is used.
"""

import argparse
import base64
import ctypes
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def probe(executable, plan, unauthorized):
    paths = []
    lock = threading.Lock()

    class Backend(BaseHTTPRequestHandler):
        def do_GET(self):
            with lock:
                paths.append(self.path)
            status = 200
            if self.path.endswith("/config/bundle"):
                body = {}
            elif self.path.endswith("/accounts/check"):
                body = {"accounts": []}
            elif self.path.endswith("/usage"):
                status = 401 if unauthorized else 200
                body = {
                    "plan_type": plan,
                    "rate_limit": {
                        "allowed": True,
                        "limit_reached": False,
                        "primary_window": {
                            "used_percent": 25,
                            "limit_window_seconds": 18000,
                            "reset_at": 1900000000,
                            "reset_after_seconds": 18000,
                        },
                    },
                }
            else:
                status, body = 404, {}
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps(body).encode())

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Backend)
    server.daemon_threads = True
    server_thread = threading.Thread(target=server.serve_forever)
    server_thread.start()
    messages = queue.Queue()
    process = None
    reader_thread = None
    refresh_requests = 0
    try:
        with tempfile.TemporaryDirectory(prefix="claudometer-codex-probe-") as directory:
            home = Path(directory)
            catalog = home / "model-catalog.json"
            catalog.write_bytes(
                (Path(__file__).resolve().parent.parent
                 / "tests/fixtures/codex-app-server/model-catalog.json").read_bytes()
            )
            environment = {
                key: os.environ[key]
                for key in ("SystemRoot", "WINDIR", "TEMP", "TMP")
                if key in os.environ
            }
            environment.update({
                key: directory
                for key in ("CODEX_HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA")
            })
            environment["RUST_LOG"] = "off"
            arguments = [str(executable), "app-server", "--stdio", "--strict-config"]
            for setting in (
                'cli_auth_credentials_store="ephemeral"',
                "analytics.enabled=false",
                'otel.exporter="none"',
                'otel.trace_exporter="none"',
                'otel.metrics_exporter="none"',
                "features.plugins=false",
                "features.remote_control=false",
                "features.runtime_metrics=false",
                "model_catalog_json=" + json.dumps(str(catalog)),
                "chatgpt_base_url=" + json.dumps(
                    f"http://127.0.0.1:{server.server_port}/backend-api/"
                ),
            ):
                arguments.extend(("-c", setting))
            process = subprocess.Popen(
                arguments, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL, cwd=home, env=environment,
                creationflags=subprocess.CREATE_NO_WINDOW,
            )
            deadline = time.monotonic() + 10

            def read_messages():
                try:
                    while line := process.stdout.readline(1048577):
                        if len(line) > 1048576 or not line.endswith(b"\n"):
                            raise ValueError("oversized protocol frame")
                        messages.put(json.loads(line))
                except (ValueError, OSError):
                    messages.put(None)
                finally:
                    messages.put(None)

            reader_thread = threading.Thread(target=read_messages)
            reader_thread.start()

            def send(message):
                process.stdin.write(json.dumps(message).encode() + b"\n")
                process.stdin.flush()

            def receive():
                nonlocal refresh_requests
                while True:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError("probe deadline exceeded")
                    message = messages.get(timeout=remaining)
                    if message is None:
                        raise RuntimeError("app-server exited or returned an invalid frame")
                    if "method" in message and "id" in message:
                        if message["method"] == "account/chatgptAuthTokens/refresh":
                            refresh_requests += 1
                        # Refuse every server request. Never return replacement tokens.
                        send({"id": message["id"], "error": {
                            "code": -32601, "message": "Host refuses server requests"
                        }})
                        continue
                    return message

            def rpc(identifier, method, params):
                send({"id": identifier, "method": method, "params": params})
                while True:
                    message = receive()
                    if message.get("id") == identifier:
                        return message

            try:
                initialized = rpc(1, "initialize", {
                    "clientInfo": {"name": "claudometer_contract_probe", "version": "0.0.0"},
                    "capabilities": {"experimentalApi": True},
                })
                if "error" in initialized:
                    raise RuntimeError("initialize failed")
                send({"method": "initialized"})
                claims = {"exp": 1900000000, "https://api.openai.com/auth": {
                    "chatgpt_plan_type": plan,
                    "chatgpt_account_id": "synthetic-account",
                    "chatgpt_user_id": "synthetic-user",
                }}
                payload = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=")
                token = "e30." + payload + ".c3ludGhldGlj"
                login = rpc(2, "account/login/start", {
                    "type": "chatgptAuthTokens", "accessToken": token,
                    "chatgptAccountId": "synthetic-account", "chatgptPlanType": plan,
                })
                if login.get("result", {}).get("type") != "chatgptAuthTokens":
                    raise RuntimeError("external token mode rejected")
                while True:
                    notification = receive()
                    if notification.get("method") == "account/login/completed":
                        login_completed = notification.get("params", {}).get("success")
                        break
                limits = rpc(3, "account/rateLimits/read", {"excludeResetCreditDetails": True})
            finally:
                # Kill descendants even after a successful response; no resident helper.
                subprocess.run(
                    [str(Path(os.environ["SystemRoot"]) / "System32/taskkill.exe"),
                     "/PID", str(process.pid), "/T", "/F"],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                    timeout=10, check=False,
                )
                process.wait(timeout=10)
                reader_thread.join(timeout=2)
                process.stdin.close()
                process.stdout.close()
            files = [file for file in home.rglob("*") if file.is_file()]
            return {
                "plan_fixture": plan,
                "usage_status_fixture": 401 if unauthorized else 200,
                "login_completed_success": login_completed,
                "limits_success": "result" in limits,
                "limits_error_code": limits.get("error", {}).get("code"),
                "refresh_requests_refused": refresh_requests,
                "local_backend_requests": paths,
                "auth_file_created": (home / "auth.json").exists(),
                "access_token_persisted": any(token.encode() in file.read_bytes() for file in files),
                "account_identifiers_persisted": any(
                    b"synthetic-account" in file.read_bytes()
                    or b"synthetic-user" in file.read_bytes() for file in files
                ),
                "reader_stopped": not reader_thread.is_alive(),
                "child_exited": process.poll() is not None,
            }
    finally:
        server.shutdown()
        server.server_close()
        server_thread.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", required=True, type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    executable = args.exe.resolve(strict=True)
    if os.name != "nt" or executable.suffix.lower() != ".exe":
        parser.error("a native Windows codex.exe is required")
    # Codex loads machine policy using the Windows known folder, even with
    # CODEX_HOME redirected. Never let a test inherit a required backend URL.
    program_data = ctypes.create_unicode_buffer(32768)
    if ctypes.windll.shell32.SHGetFolderPathW(None, 35, None, 0, program_data) != 0:
        parser.error("cannot resolve the Windows ProgramData folder")
    system_config = Path(program_data.value) / "OpenAI/Codex"
    if any((system_config / name).exists() for name in ("config.toml", "requirements.toml")):
        parser.error("machine Codex configuration exists; refusing possible network overrides")
    report = [probe(executable, plan, unauthorized)
              for plan, unauthorized in (("pro", False), ("business", False), ("pro", True))]
    text = json.dumps(report, indent=2)
    if args.report:
        args.report.write_text(text + "\n", encoding="utf-8")
    print(text)
    if any(item["auth_file_created"] or item["access_token_persisted"]
           or not item["child_exited"] or not item["reader_stopped"] for item in report):
        raise SystemExit("Probe found credential persistence or failed process cleanup")


if __name__ == "__main__":
    main()
