import type { CodeyPluginPreview } from "./codeyPlugins";

export const MAX_PLUGIN_PACKAGE_BYTES = 64 * 1024 * 1024;
const UPLOAD_CHUNK_BYTES = 256 * 1024;

type PackageRequest = (args: Record<string, unknown>) => Promise<unknown>;
type UploadedPreview = CodeyPluginPreview & { uploadId: string };

export function validateDroppedPluginFiles(files: readonly Pick<File, "name" | "size">[]): string | undefined {
  if (files.length !== 1) return "请一次拖入一个 .codey-plugin 插件包。";
  if (!files[0].name.endsWith(".codey-plugin")) return "不支持此文件格式，仅支持 .codey-plugin 插件包。";
  if (files[0].size === 0) return "插件包为空或已损坏，请重新获取文件后重试。";
  if (files[0].size > MAX_PLUGIN_PACKAGE_BYTES) return "插件包超过 64 MiB，请选择符合大小限制的文件。";
}

export async function uploadPluginPackage(file: File, request: PackageRequest, signal?: AbortSignal): Promise<UploadedPreview> {
  const invalid = validateDroppedPluginFiles([file]);
  if (invalid) throw new Error(invalid);
  let uploadId: string | undefined;
  try {
    for (let offset = 0; offset < file.size; offset += UPLOAD_CHUNK_BYTES) {
      signal?.throwIfAborted();
      const bytes = new Uint8Array(await file.slice(offset, offset + UPLOAD_CHUNK_BYTES).arrayBuffer());
      let binary = "";
      for (let start = 0; start < bytes.length; start += 16384) {
        binary += String.fromCharCode(...bytes.subarray(start, start + 16384));
      }
      signal?.throwIfAborted();
      const complete = offset + bytes.length === file.size;
      const response = await request({ upload: { id: uploadId, name: file.name, offset, contentBase64: btoa(binary), complete } }) as Partial<UploadedPreview>;
      if (!response || typeof response.uploadId !== "string") throw new Error("插件包上传响应无效，请重试检查。");
      uploadId = response.uploadId;
      signal?.throwIfAborted();
      if (complete) {
        if (typeof response.path !== "string" || !/^[a-f0-9]{64}$/i.test(response.sha256 ?? "")
          || !response.manifest?.id || !response.manifest.name || !response.manifest.version) {
          throw new Error("插件包检查响应无效，请重试检查。");
        }
        return response as UploadedPreview;
      }
    }
    throw new Error("插件包读取不完整，请重新选择文件。");
  } catch (cause) {
    if (uploadId) await request({ discardUpload: uploadId }).catch(() => undefined);
    throw cause;
  }
}

export function installPluginFileDrop(
  target: Window,
  onDrop: (files: File[]) => void,
  onActiveChange: (active: boolean) => void,
  disabled = false,
  isActive: () => boolean = () => true,
): () => void {
  const hasFiles = (event: DragEvent) => Array.from(event.dataTransfer?.types ?? []).includes("Files");
  const inZone = (event: Event) => event.composedPath().some(node =>
    typeof (node as Element).hasAttribute === "function" && (node as Element).hasAttribute("data-codey-plugin-drop-zone"));
  const reset = () => onActiveChange(false);
  const drag = (event: DragEvent) => {
    if (!isActive()) { reset(); return; }
    if (!hasFiles(event)) return;
    event.preventDefault();
    const accepted = inZone(event) && !disabled;
    if (event.dataTransfer) event.dataTransfer.dropEffect = accepted ? "copy" : "none";
    onActiveChange(accepted);
  };
  const leave = (event: DragEvent) => {
    if (!event.relatedTarget && (event.clientX <= 0 || event.clientY <= 0
      || event.clientX >= target.innerWidth || event.clientY >= target.innerHeight)) reset();
  };
  const drop = (event: DragEvent) => {
    reset();
    if (!isActive() || !hasFiles(event)) return;
    event.preventDefault();
    if (!inZone(event)) return;
    event.stopPropagation();
    if (!disabled) onDrop(Array.from(event.dataTransfer?.files ?? []));
  };
  const key = (event: KeyboardEvent) => { if (event.key === "Escape") reset(); };
  target.addEventListener("dragenter", drag, true);
  target.addEventListener("dragover", drag, true);
  target.addEventListener("dragleave", leave, true);
  target.addEventListener("drop", drop, true);
  target.addEventListener("dragend", reset, true);
  target.addEventListener("blur", reset);
  target.addEventListener("keydown", key, true);
  return () => {
    target.removeEventListener("dragenter", drag, { capture: true });
    target.removeEventListener("dragover", drag, { capture: true });
    target.removeEventListener("dragleave", leave, { capture: true });
    target.removeEventListener("drop", drop, { capture: true });
    target.removeEventListener("dragend", reset, { capture: true });
    target.removeEventListener("blur", reset);
    target.removeEventListener("keydown", key, { capture: true });
    reset();
  };
}
