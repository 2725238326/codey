use super::*;

/// Older renderers may call this endpoint. Reject before stopping Codex.
pub(in crate::commands) async fn schedule_main_process_injection_repair(
    _state: &Arc<AppState>,
) -> Result<Value, String> {
    Err("不再修改 Codex 安全开关；当前客户端可继续使用兼容模式，或通过官方插件、技能和 MCP 扩展能力".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retired_repair_never_schedules_a_restart() {
        let state = Arc::new(AppState::default());
        let _operation = state.runtime_operation.lock().await;
        for _ in 0..2 {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                schedule_main_process_injection_repair(&state),
            )
            .await
            .unwrap();
            assert!(result.unwrap_err().contains("不再修改"));
            assert!(
                !state
                    .restart_in_progress
                    .load(std::sync::atomic::Ordering::SeqCst)
            );
            assert!(state.restart_task.lock().await.is_none());
            assert!(state.runtime.lock().await.is_none());
        }
    }
}
