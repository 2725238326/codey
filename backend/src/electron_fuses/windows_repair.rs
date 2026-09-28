//! Rejects the retired elevated repair command without touching the runtime.

pub(crate) fn run_if_requested() -> Option<i32> {
    let command = std::env::args_os().nth(1)?;
    if command != "--internal-repair-codex-node-options" {
        return None;
    }
    eprintln!("已停用安全开关修改；请使用官方安装程序修复 Codex");
    Some(21)
}
