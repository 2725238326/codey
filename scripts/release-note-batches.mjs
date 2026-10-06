const DEFAULT_MAX_BYTES = 48 * 1024;

function serializedBytes(value) {
  return Buffer.byteLength(JSON.stringify(value), "utf8");
}

function splitDiffLines(diff) {
  if (diff.length === 0) return [""];
  const lines = [];
  let start = 0;
  while (start < diff.length) {
    const end = diff.indexOf("\n", start);
    if (end < 0) {
      lines.push(diff.slice(start));
      break;
    }
    lines.push(diff.slice(start, end + 1));
    start = end + 1;
  }
  return lines;
}

function validateMaxBytes(maxBytes) {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) throw new Error("单批大小必须是正整数");
}

export function chunkPatches(patches, maxBytes = DEFAULT_MAX_BYTES) {
  validateMaxBytes(maxBytes);
  if (!Array.isArray(patches)) throw new TypeError("差异补丁必须是数组");

  const chunks = [];
  for (const patch of patches) {
    if (!patch || typeof patch.file !== "string" || typeof patch.diff !== "string") throw new TypeError("差异补丁必须包含文件路径和文本差异");
    let fragment = "";
    for (const line of splitDiffLines(patch.diff)) {
      const candidate = fragment + line;
      const entry = { file: patch.file, diff: candidate };
      if (serializedBytes([entry]) > maxBytes) {
        if (!fragment) throw new Error(`单行差异与文件路径序列化后超过单批大小限制：${patch.file}`);
        chunks.push({ file: patch.file, diff: fragment });
        fragment = line;
        if (serializedBytes([{ file: patch.file, diff: fragment }]) > maxBytes) throw new Error(`单行差异与文件路径序列化后超过单批大小限制：${patch.file}`);
      } else {
        fragment = candidate;
      }
    }
    chunks.push({ file: patch.file, diff: fragment });
  }

  const batches = [];
  let batch = [];
  for (const chunk of chunks) {
    const candidate = [...batch, chunk];
    if (serializedBytes(candidate) > maxBytes) {
      if (batch.length) batches.push(batch);
      batch = [chunk];
    } else {
      batch = candidate;
    }
  }
  if (batch.length) batches.push(batch);
  return batches;
}

export function mergeNoteResults(results) {
  if (!Array.isArray(results)) throw new TypeError("日志结果必须是数组");
  const notes = [];
  const evidence = [];
  const seen = new Set();
  for (const result of results) {
    if (!result || typeof result.notes !== "string" || !Array.isArray(result.evidence)) throw new Error("分批日志结果无效");
    if (result.notes === "" && result.evidence.length === 0) continue;
    const lines = result.notes.trim().split(/\r?\n/).filter((line) => line.trim());
    if (lines.length !== result.evidence.length) throw new Error("分批日志与差异证据数量不一致");
    for (const [index, line] of lines.entries()) {
      if (!line.startsWith("- ") || typeof result.evidence[index]?.note !== "string" || result.evidence[index].note !== line.slice(2)) throw new Error("分批日志与差异证据不一致");
      const note = result.evidence[index].note;
      if (seen.has(note)) continue;
      seen.add(note);
      notes.push(`- ${note}`);
      evidence.push(result.evidence[index]);
    }
  }
  return { notes: notes.join("\n"), evidence };
}
