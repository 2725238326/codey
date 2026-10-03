"""Verify the real native ABI and package using dummy keys; no network calls."""
import argparse
import concurrent.futures
import ctypes
import hashlib
import json
from pathlib import Path
import tempfile
import zipfile


class Buffer(ctypes.Structure):
    _fields_ = [("data", ctypes.c_void_p), ("len", ctypes.c_size_t)]


CREATE = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_void_p, ctypes.c_size_t,
                         ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(Buffer))
INVOKE = ctypes.CFUNCTYPE(ctypes.c_int32, ctypes.c_void_p, ctypes.c_void_p,
                         ctypes.c_size_t, ctypes.POINTER(Buffer))
DESTROY = ctypes.CFUNCTYPE(None, ctypes.c_void_p)
FREE = ctypes.CFUNCTYPE(None, Buffer)


class Api(ctypes.Structure):
    _fields_ = [("abi_version", ctypes.c_uint32), ("struct_size", ctypes.c_uint32),
                ("create", CREATE), ("invoke", INVOKE),
                ("destroy", DESTROY), ("free_buffer", FREE)]


class Native:
    def __init__(self, library, config, root):
        self.library = ctypes.CDLL(str(library))
        self.library.codey_plugin_entry_v1.restype = ctypes.POINTER(Api)
        self.api = self.library.codey_plugin_entry_v1().contents
        assert self.api.abi_version == 1 and self.api.struct_size == ctypes.sizeof(Api)
        self.instance = ctypes.c_void_p()
        for name in ("data", "logs"):
            (root / name).mkdir(exist_ok=True)
        payload = json.dumps({"config": config, "context": {
            "pluginId": "dev.codey.rixin", "pluginDir": str(root),
            "dataDir": str(root / "data"), "logDir": str(root / "logs")
        }}).encode()
        output = Buffer()
        rc = self.api.create(payload, len(payload), ctypes.byref(self.instance), ctypes.byref(output))
        result = self.decode(output)
        if rc:
            raise ValueError(result)

    def decode(self, output):
        try:
            raw = ctypes.string_at(output.data, output.len) if output.len else b"null"
            return json.loads(raw)
        finally:
            self.api.free_buffer(output)

    def invoke(self, method, params):
        payload = json.dumps({"method": method, "params": params}).encode()
        output = Buffer()
        rc = self.api.invoke(self.instance, payload, len(payload), ctypes.byref(output))
        result = self.decode(output)
        if rc:
            raise ValueError(result)
        return result

    def close(self):
        if self.instance:
            self.api.destroy(self.instance)
            self.instance = ctypes.c_void_p()


def event(selected=False, authorized=True):
    return {"stage": "beforeSend", "attempt": int(selected), "metadata": {
        "apiKeyAuthorized": authorized, "apiKeySelected": selected,
        "officialAccount": False,
        "upstreamUrl": "https://token.sensenova.cn/v1/responses"
    }}


def verify_library(library):
    with tempfile.TemporaryDirectory(prefix="rixin-abi-") as directory:
        root = Path(directory).resolve()
        for config, expected in [({"apiKey": "one"}, ["one"] * 7),
                                 ({"apiKeys": ["a", "b", "c"]}, ["a", "b", "c", "a", "b", "c", "a"])]:
            plugin = Native(library, config, root)
            try:
                route = plugin.invoke("provider.describe", {})
                assert route["baseUrl"] == "https://token.sensenova.cn/v1/responses"
                assert route["upstreamProtocol"] == "openaiResponses" and route["headers"] == []
                for key in expected:
                    assert plugin.invoke("request.beforeSend", event())["apiKey"] == key
                    assert "apiKey" not in plugin.invoke("request.beforeSend", event(selected=True))
                    assert "apiKey" not in plugin.invoke("request.beforeSend", event(authorized=False))
            finally:
                plugin.close()
        plugin = Native(library, {"apiKeys": ["a", "b", "c"]}, root)
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=16) as executor:
                results = list(executor.map(lambda _: plugin.invoke("request.beforeSend", event())["apiKey"], range(600)))
            # Native SDK locking is exercised here, without a Python-side mutex.
            assert {key: results.count(key) for key in ("a", "b", "c")} == {"a": 200, "b": 200, "c": 200}
            assert plugin.invoke("request.beforeSend", event())["apiKey"] == "a"
        finally:
            plugin.close()
        for config in [{}, {"apiKey": ""}, {"apiKeys": []}, {"apiKeys": [None]},
                       {"apiKey": 10}, {"apiKey": "secret-fixture", "apiKeys": ["a"]}]:
            try:
                plugin = Native(library, config, root)
            except ValueError as error:
                assert "secret-fixture" not in str(error)
            else:
                plugin.close()
                raise AssertionError("Invalid configuration was accepted")
        log = (root / "logs" / "plugin.log").read_text(encoding="utf-8")
        assert "secret-fixture" not in log and "apiKey" not in log
    print("Native ABI: single/multiple keys, retry, 600 concurrent calls, invalid config PASS")


def verify_package(path):
    with zipfile.ZipFile(path) as package:
        manifest = json.loads(package.read("manifest.json"))
        assert set(package.namelist()) == {"manifest.json", "config.json", manifest["entry"]}
        assert manifest["abiVersion"] == 1
        assert manifest["platform"] == "windows" and manifest["arch"] == "x86_64"
        assert hashlib.sha256(package.read(manifest["entry"])).hexdigest() == manifest["librarySha256"]
        assert set(manifest["capabilities"]) == {"provider.route.v1", "request.lifecycle.v1", "request.lifecycle.api_key"}
        assert manifest["apiKeyUrls"] == ["https://token.sensenova.cn/v1/responses", "https://token.sensenova.cn/v1/responses/compact"]
        assert len(package.read("config.json")) <= 1024 * 1024
        assert json.loads(package.read("config.json"))["apiKeys"] == [""]
    print("Package: file set, platform, ABI, capabilities, scoped URLs and SHA-256 PASS")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--library", type=Path)
    parser.add_argument("--package", type=Path)
    args = parser.parse_args()
    if not args.library and not args.package:
        parser.error("provide --library or --package")
    if args.library:
        verify_library(args.library.resolve())
    if args.package:
        verify_package(args.package)
