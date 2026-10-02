use super::*;

#[derive(Default)]
pub(crate) struct ModelHotReloadOutcome {
    pub(crate) reloaded: bool,
    pub(crate) deferred: bool,
    pub(crate) error: Option<String>,
}

impl ModelHotReloadOutcome {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            error: Some(error.into()),
            ..Self::default()
        }
    }

    pub(crate) fn add_to_response(self, mut response: Value) -> Value {
        if let Some(object) = response.as_object_mut() {
            object.insert("modelHotReloaded".into(), Value::Bool(self.reloaded));
            if self.deferred {
                object.insert("modelHotReloadDeferred".into(), Value::Bool(true));
            }
            if let Some(error) = self.error {
                object.insert("modelHotReloadError".into(), Value::String(error));
            }
        }
        response
    }
}

pub(crate) fn model_catalog_config_for_runtime<'a>(
    current: &'a CodeyConfig,
    runtime_applied: Option<&'a CodeyConfig>,
    applied_catalog: Option<&'a CodeyConfig>,
) -> &'a CodeyConfig {
    applied_catalog.or(runtime_applied).unwrap_or(current)
}

pub(crate) async fn runtime_renderer_model_catalog(state: &Arc<AppState>) -> Result<Value, String> {
    let _delivery = state.model_delivery_lock.lock().await;
    let runtime = state.runtime.lock().await.clone();
    let applied = match runtime.as_ref() {
        Some(runtime) => Some(runtime.applied_model_catalog_config().await),
        None => None,
    };
    let current = state.config.read().await.clone();
    let config = model_catalog_config_for_runtime(
        &current,
        runtime.as_ref().map(|runtime| &runtime.applied_config),
        applied.as_ref(),
    );
    current_renderer_model_catalog_async(config.clone()).await
}

async fn refresh_current_renderer(
    runtime: &crate::launcher::CodeyRuntime,
    catalog: &Value,
) -> Result<cdp::ModelWhitelistRefresh, String> {
    let mut websocket_url = runtime.renderer_websocket_url().await;
    for _ in 0..2 {
        let result = cdp::refresh_model_whitelist(&websocket_url, catalog).await;
        let current_url = runtime.renderer_websocket_url().await;
        if Arc::ptr_eq(&websocket_url, &current_url) {
            return result.map_err(|error| format!("{error:#}"));
        }
        websocket_url = current_url;
    }
    Err("模型列表更新期间页面连续重连，请重试同步".into())
}

pub(crate) async fn hot_reload_runtime_models(state: &Arc<AppState>) -> ModelHotReloadOutcome {
    let _runtime_operation = state.runtime_operation.lock().await;
    let _delivery = state.model_delivery_lock.lock().await;
    if state.is_shutting_down() {
        return ModelHotReloadOutcome::default();
    }
    let Some(runtime) = state.runtime.lock().await.clone() else {
        return ModelHotReloadOutcome::default();
    };
    let config = state.config.read().await.clone();
    if runtime.applied_config.local_router_enabled != config.local_router_enabled {
        return ModelHotReloadOutcome::default();
    }
    let delivered =
        if runtime_supports_current_routes_for_hot_reload(&runtime.applied_config, &config) {
            config
        } else {
            config_with_launch_pinned_transport(&runtime.applied_config, &config)
        };
    let expected_catalog = match current_renderer_model_catalog_async(delivered.clone()).await {
        Ok(catalog) => catalog,
        Err(error) => return ModelHotReloadOutcome::failed(error),
    };
    let previous = runtime.applied_model_catalog_config().await;
    let router_swap = if delivered.local_router_enabled {
        match runtime.sync_local_router_routes(&delivered) {
            Ok(swap) => swap,
            Err(error) => return ModelHotReloadOutcome::failed(format!("{error:#}")),
        }
    } else {
        None
    };
    match refresh_current_renderer(&runtime, &expected_catalog).await {
        Ok(refresh) => {
            runtime.mark_model_config_applied(&delivered).await;
            ModelHotReloadOutcome {
                reloaded: true,
                deferred: refresh.deferred,
                error: None,
            }
        }
        Err(error) => {
            if let Some(swap) = router_swap {
                runtime.revert_local_router_routes(swap);
            }
            let rollback = match current_renderer_model_catalog_async(previous).await {
                Ok(catalog) => refresh_current_renderer(&runtime, &catalog)
                    .await
                    .map(|_| ()),
                Err(error) => Err(error),
            };
            let error = match rollback {
                Ok(()) => error,
                Err(rollback_error) => {
                    format!("{error}；恢复原模型列表失败：{rollback_error}")
                }
            };
            error_log::record_failure(
                "patch_verification_failed",
                "refresh_model_whitelist",
                error.clone(),
                json!({
                    "modelCount": expected_catalog.get("models").and_then(Value::as_array).map(Vec::len),
                    "websocketUrl": runtime.renderer_websocket_url().await,
                }),
            );
            ModelHotReloadOutcome::failed(error)
        }
    }
}

#[cfg(test)]
#[path = "delivery_tests.rs"]
mod tests;
