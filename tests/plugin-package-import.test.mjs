import assert from "node:assert/strict";
import { File } from "node:buffer";
import test from "node:test";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const { MAX_PLUGIN_PACKAGE_BYTES, validateDroppedPluginFiles, uploadPluginPackage, installPluginFileDrop } = await loadTypeScriptModule(new URL("../src/pluginPackageImport.ts", import.meta.url));
const preview = { uploadId: "upload-demo", path: "staged.codey-plugin", sha256: "a".repeat(64), manifest: { id: "demo", name: "Demo", version: "1.0.0" } };
const zone = { hasAttribute: name => name === "data-codey-plugin-drop-zone" };

function dragEvent(type, files = [], { inside = true, types = ["Files"], ...values } = {}) {
  const event = new Event(type, { cancelable: true });
  Object.assign(event, { dataTransfer: { types, files, dropEffect: "none" }, clientX: 100, clientY: 100, relatedTarget: null, ...values });
  event.composedPath = () => inside ? [{}, zone, {}] : [{}];
  return event;
}

function dropHarness(disabled = false) {
  const target = Object.assign(new EventTarget(), { innerWidth: 1000, innerHeight: 800 });
  const imports = [], highlights = [];
  const dispose = installPluginFileDrop(target, files => imports.push(files), active => highlights.push(active), disabled);
  const send = (type, files, options) => { const event = dragEvent(type, files, options); target.dispatchEvent(event); return event; };
  return { target, imports, highlights, dispose, send };
}

test("drop validation preserves the existing format and 64 MiB boundary", () => {
  const file = { name: "demo.codey-plugin", size: 1 };
  assert.equal(validateDroppedPluginFiles([file]), undefined);
  assert.equal(validateDroppedPluginFiles([{ ...file, size: MAX_PLUGIN_PACKAGE_BYTES }]), undefined);
  assert.match(validateDroppedPluginFiles([{ ...file, size: MAX_PLUGIN_PACKAGE_BYTES + 1 }]), /64 MiB/);
  assert.match(validateDroppedPluginFiles([{ ...file, size: 0 }]), /为空或已损坏/);
  for (const name of ["demo.zip", "demo.json", "demo.dll", "demo.CODEY-PLUGIN", "demo.codey-plugin.txt"]) assert.match(validateDroppedPluginFiles([{ ...file, name }]), /仅支持/);
  for (const files of [[], [file, file]]) assert.match(validateDroppedPluginFiles(files), /一次/);
});

test("file drag highlights nested Shadow DOM drop zones and imports only on release", () => {
  const harness = dropHarness();
  const file = new File(["data"], "demo.codey-plugin");
  assert.equal(harness.send("dragenter", [file]).defaultPrevented, true);
  const over = harness.send("dragover", [file]);
  assert.equal(over.defaultPrevented, true);
  assert.equal(over.dataTransfer.dropEffect, "copy");
  harness.send("dragleave", [file]);
  assert.equal(harness.highlights.at(-1), true);
  assert.equal(harness.imports.length, 0);
  const drop = harness.send("drop", [file]);
  assert.equal(drop.defaultPrevented, true);
  assert.deepEqual(harness.imports, [[file]]);
  assert.equal(harness.highlights.at(-1), false);
  harness.dispose();
});

test("cancel, exit, blur and Escape remove highlighting without importing", () => {
  const harness = dropHarness();
  for (const cancel of ["dragend", "blur", "keydown", "dragleave"]) {
    harness.send("dragenter");
    assert.equal(harness.highlights.at(-1), true);
    harness.send(cancel, [], { key: "Escape", clientX: 0 });
    assert.equal(harness.highlights.at(-1), false);
  }
  harness.send("dragenter");
  harness.send("dragover", [], { inside: false });
  assert.equal(harness.highlights.at(-1), false);
  assert.equal(harness.imports.length, 0);
  harness.dispose();
});

test("outside and busy drops block file navigation but never import, text drags stay untouched", () => {
  for (const disabled of [false, true]) {
    const harness = dropHarness(disabled);
    assert.equal(harness.send("drop", [], { inside: false }).defaultPrevented, true);
    if (disabled) {
      assert.equal(harness.send("dragover").dataTransfer.dropEffect, "none");
      assert.equal(harness.send("drop").defaultPrevented, true);
      assert.equal(harness.highlights.at(-1), false);
    }
    assert.equal(harness.send("dragover", [], { types: ["text/plain"] }).defaultPrevented, false);
    assert.equal(harness.send("drop", [], { types: ["text/plain"] }).defaultPrevented, false);
    assert.equal(harness.imports.length, 0);
    harness.dispose();
    assert.equal(harness.send("drop").defaultPrevented, false);
  }
});

test("pathless browser files are uploaded in bounded binary-exact chunks through inspection", async () => {
  const bytes = Uint8Array.from({ length: 600000 }, (_, index) => index % 256);
  const file = new File([bytes], "demo.codey-plugin");
  const calls = [];
  const result = await uploadPluginPackage(file, async args => {
    calls.push(args);
    return args.upload.complete ? preview : { uploadId: preview.uploadId };
  });
  assert.equal(result, preview);
  assert.equal(calls.length, 3);
  assert.equal(calls[0].upload.id, undefined);
  assert.deepEqual(calls.map(call => call.upload.offset), [0, 262144, 524288]);
  assert.deepEqual(calls.map(call => call.upload.complete), [false, false, true]);
  assert.ok(calls.slice(1).every(call => call.upload.id === preview.uploadId));
  assert.deepEqual(Buffer.concat(calls.map(call => Buffer.from(call.upload.contentBase64, "base64"))), Buffer.from(bytes));
  assert.ok(calls.every(call => call.upload.contentBase64.length < 350000));
});

test("invalid files never upload, corruption and bridge errors release staging and allow retry", async () => {
  for (const file of [new File(["x"], "demo.zip"), new File([], "demo.codey-plugin")]) {
    let calls = 0;
    await assert.rejects(uploadPluginPackage(file, async () => { calls++; }), /格式|为空/);
    assert.equal(calls, 0);
  }
  const file = new File([new Uint8Array(300000)], "demo.codey-plugin");
  const calls = [];
  await assert.rejects(uploadPluginPackage(file, async args => {
    calls.push(args);
    if (args.discardUpload) return { status: "ok" };
    if (args.upload.complete) throw new Error("ZIP 格式无效");
    return { uploadId: preview.uploadId };
  }), /ZIP/);
  assert.deepEqual(calls.at(-1), { discardUpload: preview.uploadId });
  const result = await uploadPluginPackage(file, async args => args.upload.complete ? preview : { uploadId: preview.uploadId });
  assert.equal(result.sha256, preview.sha256);
});

test("aborting an in-flight upload discards its token and malformed previews never install", async () => {
  const controller = new AbortController();
  const calls = [];
  await assert.rejects(uploadPluginPackage(new File(["x"], "demo.codey-plugin"), async args => {
    calls.push(args);
    if (args.discardUpload) return { status: "ok" };
    controller.abort();
    return preview;
  }, controller.signal), /abort/i);
  assert.deepEqual(calls.at(-1), { discardUpload: preview.uploadId });
  for (const invalid of [{ ...preview, sha256: "invalid" }, { ...preview, manifest: {} }, null]) {
    const requests = [];
    await assert.rejects(uploadPluginPackage(new File(["x"], "demo.codey-plugin"), async args => {
      requests.push(args);
      return args.discardUpload ? { status: "ok" } : invalid;
    }), /响应无效/);
    if (invalid) assert.deepEqual(requests.at(-1), { discardUpload: preview.uploadId });
  }
});
