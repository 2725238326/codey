import { useEffect, useState } from "react";
import { IconAlertTriangle, IconFilePlus, IconFolderOpen, IconPuzzle } from "@tabler/icons-react";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
} from "./components/ui";
import type { CodeyPlugin, CodeyPluginPreview } from "./codeyPlugins";

function packageFileName(path: string) {
  const parts = path.split(/[/\\]/);
  return parts[parts.length - 1] || path;
}

export function PluginImportDialog({
  open,
  container,
  platform,
  busy,
  preview,
  upgrading,
  error,
  onClose,
  onSelectFile,
  onInspectPath,
  onClearPreview,
  onConfirm,
}: {
  open: boolean;
  container?: HTMLElement | null;
  platform?: string;
  busy: boolean;
  preview: CodeyPluginPreview | null;
  upgrading?: CodeyPlugin;
  error: string;
  onClose: () => void;
  onSelectFile: () => void;
  onInspectPath: (path: string) => void;
  onClearPreview: () => void;
  onConfirm: () => void;
}) {
  const nativePicker = platform !== "linux";
  const [path, setPath] = useState("");
  useEffect(() => {
    if (!open) setPath("");
    else if (preview?.path) setPath(preview.path);
  }, [open, preview?.path]);

  const capabilities = [
    ...(preview?.manifest.capabilities ?? []),
    ...(preview?.manifest.permissions ?? []),
  ];

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !busy) onClose();
      }}
    >
      <DialogContent
        container={container}
        className="max-w-[520px]"
        onEscapeKeyDown={(event) => {
          if (busy) event.preventDefault();
        }}
        onPointerDownOutside={(event) => {
          if (busy) event.preventDefault();
        }}
      >
        <DialogHeader>
          <div className="flex items-start gap-3.5">
            <div className="flex size-10 shrink-0 items-center justify-center rounded-xl bg-blue-500/10 text-blue-600 dark:bg-blue-500/15 dark:text-blue-400">
              <IconFilePlus size={20} stroke={1.75} aria-hidden="true" />
            </div>
            <div className="min-w-0 flex-1 space-y-1">
              <DialogTitle>导入插件包</DialogTitle>
              <DialogDescription>
                选择本地 .codey-plugin 文件，确认信息后导入。安装后默认停用。
              </DialogDescription>
            </div>
          </div>
        </DialogHeader>

        <div className="space-y-3.5 pt-2 text-xs">
          <div className="rounded-xl border border-dashed border-black/[0.12] bg-black/[0.02] p-3.5 dark:border-white/[0.12] dark:bg-white/[0.03]">
            {nativePicker ? (
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <div className="font-medium text-foreground">
                    {preview ? packageFileName(preview.path) : "尚未选择插件包"}
                  </div>
                  <p className="mb-0 mt-0.5 truncate font-mono text-[11px] text-muted">
                    {preview ? preview.path : "支持 .codey-plugin 安装包"}
                  </p>
                </div>
                <Button
                  size="sm"
                  variant={preview ? "outline" : "default"}
                  disabled={busy}
                  onClick={onSelectFile}
                >
                  <IconFolderOpen size={14} aria-hidden="true" />
                  <span>{preview ? "重新选择" : "选择文件"}</span>
                </Button>
              </div>
            ) : (
              <div className="space-y-2">
                <div className="font-medium text-foreground">插件包路径</div>
                <div className="flex gap-2">
                  <Input
                    aria-label="插件包路径"
                    placeholder=".codey-plugin 文件的完整路径"
                    value={path}
                    disabled={busy}
                    onChange={(event) => {
                      setPath(event.target.value);
                      if (preview) onClearPreview();
                    }}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" && path.trim() && !busy) {
                        event.preventDefault();
                        onInspectPath(path.trim());
                      }
                    }}
                    className="flex-1 text-xs"
                  />
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={busy || !path.trim()}
                    onClick={() => onInspectPath(path.trim())}
                  >
                    检查安装包
                  </Button>
                </div>
              </div>
            )}
          </div>

          {preview ? (
            <div className="space-y-3">
              <div className="flex items-center gap-3 rounded-xl border border-black/[0.08] bg-black/[0.02] p-3 dark:border-white/[0.08] dark:bg-white/[0.03]">
                <div className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-black/[0.05] text-foreground/70 dark:bg-white/[0.08] dark:text-foreground/80">
                  <IconPuzzle size={18} stroke={1.75} aria-hidden="true" />
                </div>
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="truncate text-xs font-semibold text-foreground">
                      {preview.manifest.name}
                    </span>
                    <span className="rounded bg-black/[0.05] px-1.5 py-0.5 font-mono text-[10px] text-muted dark:bg-white/[0.08]">
                      {upgrading
                        ? `${upgrading.version} → ${preview.manifest.version}`
                        : `v${preview.manifest.version}`}
                    </span>
                    <span className="rounded-md bg-blue-500/10 px-1.5 py-0.5 text-[10px] font-medium text-blue-600 dark:text-blue-400">
                      {upgrading ? "升级" : "新安装"}
                    </span>
                  </div>
                  <p className="mt-0.5 mb-0 truncate font-mono text-[11px] text-muted">
                    {preview.manifest.id}
                  </p>
                </div>
              </div>

              {preview.manifest.description ? (
                <p className="m-0 leading-relaxed text-foreground/80">
                  {preview.manifest.description}
                </p>
              ) : null}

              <div className="space-y-1.5 rounded-xl border border-black/[0.08] bg-black/[0.015] p-3 dark:border-white/[0.08] dark:bg-white/[0.02]">
                <div className="font-medium text-foreground">声明能力</div>
                {capabilities.length > 0 ? (
                  <div className="flex flex-wrap gap-1.5 pt-0.5">
                    {capabilities.map((capability) => (
                      <span
                        key={capability}
                        className="inline-flex items-center rounded-md bg-black/[0.05] px-2 py-0.5 font-mono text-[11px] text-muted dark:bg-white/[0.08]"
                      >
                        {capability}
                      </span>
                    ))}
                  </div>
                ) : (
                  <p className="m-0 text-muted">无特定权限声明</p>
                )}
                {preview.manifest.headerNames && preview.manifest.headerNames.length > 0 ? (
                  <div className="pt-1 text-muted">
                    <strong className="font-medium text-foreground">可修改请求头：</strong>
                    {preview.manifest.headerNames.join("、")}
                  </div>
                ) : null}
                {capabilities.includes("provider.account.v1") ? (
                  <p className="m-0 pt-1 leading-relaxed text-muted">
                    启用后，插件将接收其线路的请求正文，以及配置邮箱对应账号的访问令牌；不会接收刷新令牌。
                  </p>
                ) : null}
              </div>

              <div className="flex items-start gap-2.5 rounded-xl border border-amber-500/25 bg-amber-500/10 p-3 text-amber-800 dark:text-amber-300">
                <IconAlertTriangle className="mt-0.5 shrink-0" size={16} aria-hidden="true" />
                <p className="m-0 leading-relaxed">
                  原生插件与 Codey 同进程运行，具有相同系统权限。请确认来源安全。
                </p>
              </div>
            </div>
          ) : null}

          {error ? (
            <div
              role="alert"
              className="flex items-start gap-2 rounded-xl border border-red-200/60 bg-red-50/70 p-3 text-red-700 dark:border-red-900/60 dark:bg-red-950/40 dark:text-red-300"
            >
              <IconAlertTriangle size={15} className="mt-0.5 shrink-0" />
              <div className="min-w-0 flex-1 break-words">{error}</div>
            </div>
          ) : null}
        </div>

        <DialogFooter className="mt-5 pt-1">
          <Button variant="outline" size="sm" disabled={busy} onClick={onClose}>
            取消
          </Button>
          <Button
            size="sm"
            disabled={busy || !preview}
            onClick={onConfirm}
          >
            {upgrading ? "确认升级" : "确认导入"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
