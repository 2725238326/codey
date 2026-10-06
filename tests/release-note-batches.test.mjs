import assert from "node:assert/strict";
import test from "node:test";
import { chunkPatches, mergeNoteResults } from "../scripts/release-note-batches.mjs";

function byteLength(value) {
  return Buffer.byteLength(JSON.stringify(value), "utf8");
}

function assertReassembled(patches, batches) {
  const chunks = batches.flat();
  let offset = 0;
  for (const patch of patches) {
    let diff = "";
    let consumed = false;
    while (offset < chunks.length && chunks[offset].file === patch.file && (!consumed || diff.length < patch.diff.length)) {
      diff += chunks[offset++].diff;
      consumed = true;
    }
    assert.equal(diff, patch.diff);
  }
  assert.equal(offset, chunks.length);
}

test("splits large patches without changing file paths or diff contents", () => {
  const patches = [
    { file: "src/alpha.js", diff: "diff --git a/src/alpha.js b/src/alpha.js\r\n@@ -1,2 +1,5 @@\r\n-old\r\n+new\r\n+增加内容\r\n" },
    { file: "src/beta.js", diff: "@@ -4,1 +4,3 @@\n+beta-one\n+beta-two\n" },
  ];
  const batches = chunkPatches(patches, 90);
  assert.ok(batches.length > 1);
  for (const batch of batches) assert.ok(byteLength(batch) <= 90);
  assertReassembled(patches, batches);
});

test("supports Unicode, CRLF, more than one hundred files, and total diffs above 80 KB", () => {
  const patches = Array.from({ length: 125 }, (_, index) => ({ file: `src/文件-${index}.ts`, diff: `@@ -1,1 +1,41 @@\r\n${`+第 ${index} 行，包含中文内容和转义字符 \\\"\r\n`.repeat(40)}` }));
  const batches = chunkPatches(patches, 512);
  assert.ok(patches.length > 100);
  assert.ok(Buffer.byteLength(patches.map(patch => patch.diff).join("")) > 80_000);
  assert.ok(batches.length > 1);
  for (const batch of batches) assert.ok(byteLength(batch) <= 512);
  assertReassembled(patches, batches);
});

test("splits one large file at complete diff lines", () => {
  const patch = { file: "large.txt", diff: Array.from({ length: 5000 }, (_, index) => `+line-${index}\n`).join("") };
  const batches = chunkPatches([patch], 2048);
  assert.ok(batches.length > 1);
  for (const batch of batches) assert.ok(byteLength(batch) <= 2048);
  assertReassembled([patch], batches);
});

test("rejects a line that cannot fit in a single serialized batch", () => {
  const patch = { file: "secrets.txt", diff: `+${"超长".repeat(100)}\n` };
  assert.throws(() => chunkPatches([patch], 64), /单行差异与文件路径序列化后超过单批大小限制/);
});

test("empty diffs remain representable and empty input returns no batches", () => {
  assert.deepEqual(chunkPatches([]), []);
  const patches = [{ file: "empty.txt", diff: "" }];
  const batches = chunkPatches(patches, 128);
  assertReassembled(patches, batches);
  assert.equal(byteLength(batches[0]), byteLength([{ file: "empty.txt", diff: "" }]));
});

test("deduplicates exact note text while preserving its evidence", () => {
  const firstEvidence = { note: "修复启动流程", file: "src/start.js", excerpt: "+start()" };
  const duplicateEvidence = { note: "修复启动流程", file: "src/other.js", excerpt: "+same" };
  const secondEvidence = { note: "增加更新检查", file: "src/update.js", excerpt: "+check()" };
  assert.deepEqual(mergeNoteResults([
    { notes: "- 修复启动流程\n- 增加更新检查", evidence: [firstEvidence, secondEvidence] },
    { notes: "- 修复启动流程", evidence: [duplicateEvidence] },
  ]), { notes: "- 修复启动流程\n- 增加更新检查", evidence: [firstEvidence, secondEvidence] });
});

test("ignores empty batch results", () => {
  assert.deepEqual(mergeNoteResults([{ notes: "", evidence: [] }]), { notes: "", evidence: [] });
  for (const result of [null, { notes: "", evidence: [{}] }, { notes: "- 无证据结论", evidence: [] }]) assert.throws(() => mergeNoteResults([result]));
});
