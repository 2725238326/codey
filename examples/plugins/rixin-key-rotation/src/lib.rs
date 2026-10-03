use codey_plugin_sdk::{
    Plugin, PluginContext,
    provider::RouteDescriptor,
    serde_json::{Value, json},
};
use std::collections::HashSet;

const BASE_URL: &str = "https://token.sensenova.cn/v1/responses";
const RESPONSE_URLS: &[&str] = &[
    "https://token.sensenova.cn/v1/responses",
    "https://token.sensenova.cn/v1/responses/compact",
];

// The SDK serializes calls to this instance with its own mutex. Selecting and
// advancing live in the same &mut callback, before any upstream I/O begins.
struct RixinPlugin {
    keys: Vec<String>,
    next: usize,
    models: Vec<String>,
}

impl RixinPlugin {
    fn from_config(config: Value) -> Result<Self, String> {
        let object = config.as_object().ok_or("插件配置必须是 JSON 对象")?;
        for field in object.keys() {
            if !["apiKey", "apiKeys", "models", "_comments"].contains(&field.as_str()) {
                return Err("插件配置包含不支持的字段；仅支持 apiKey、apiKeys 和 models".into());
            }
        }
        if object.contains_key("apiKey") && object.contains_key("apiKeys") {
            return Err("apiKey 与 apiKeys 只能配置其中一种".into());
        }
        let raw_keys: Vec<&Value> = if let Some(keys) = object.get("apiKeys") {
            let keys = keys.as_array().ok_or("apiKeys 必须是非空字符串数组")?;
            if keys.is_empty() || keys.len() > 1024 {
                return Err("apiKeys 必须包含 1 到 1024 个 Key".into());
            }
            keys.iter().collect()
        } else if let Some(key) = object.get("apiKey") {
            vec![key]
        } else {
            return Err("未配置 API Key：请填写 apiKey 或 apiKeys".into());
        };
        let mut keys = Vec::with_capacity(raw_keys.len());
        for (index, value) in raw_keys.into_iter().enumerate() {
            let key = value
                .as_str()
                .ok_or_else(|| format!("第 {} 个 API Key 必须是非空字符串", index + 1))?
                .trim();
            if key.is_empty() || key.len() > 8192 || !key.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(format!("第 {} 个 API Key 为空或包含无效字符", index + 1));
            }
            keys.push(key.to_owned());
        }
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        if let Some(values) = object.get("models") {
            let values = values.as_array().ok_or("models 必须是非空字符串数组")?;
            if values.is_empty() || values.len() > 32 {
                return Err("models 必须包含 1 到 32 个模型 ID".into());
            }
            for value in values {
                let model = value.as_str().ok_or("模型 ID 必须是字符串")?.trim();
                if model.is_empty()
                    || model.chars().count() > 128
                    || model.chars().any(|c| c.is_control() || c.is_whitespace())
                    || model.eq_ignore_ascii_case("codex-auto-review")
                    || !seen.insert(model.to_ascii_lowercase())
                {
                    return Err("模型 ID 无效或重复".into());
                }
                models.push(model.to_owned());
            }
        } else {
            models.push("deepseek-v4-flash".into());
        }
        Ok(Self {
            keys,
            next: 0,
            models,
        })
    }

    fn before_send(&mut self, params: Value) -> Result<Value, String> {
        let metadata = params.get("metadata").ok_or("缺少宿主生命周期 metadata")?;
        // Unrelated routes do not advance the cursor or receive credentials.
        if metadata.get("apiKeyAuthorized").and_then(Value::as_bool) != Some(true) {
            return Ok(json!({"action":"continue"}));
        }
        if params.get("stage").and_then(Value::as_str) != Some("beforeSend")
            || metadata.get("officialAccount").and_then(Value::as_bool) != Some(false)
            || !metadata
                .get("upstreamUrl")
                .and_then(Value::as_str)
                .is_some_and(|url| RESPONSE_URLS.contains(&url))
        {
            return Err("API Key 轮换仅适用于日日新原生 Responses 目标".into());
        }
        match metadata.get("apiKeySelected").and_then(Value::as_bool) {
            Some(true) => Ok(json!({"action":"continue"})),
            Some(false) => {
                if params.get("attempt").and_then(Value::as_u64) != Some(0) {
                    return Err("重试请求必须复用首次发送选定的 API Key".into());
                }
                let key = &self.keys[self.next];
                let result = json!({"action":"continue", "apiKey":key});
                self.next = (self.next + 1) % self.keys.len();
                Ok(result)
            }
            None => Err("宿主缺少 apiKeySelected；请使用支持 Key 轮换的 Codey".into()),
        }
    }
}

impl Plugin for RixinPlugin {
    fn create(config: Value, context: PluginContext) -> Result<Self, String> {
        let plugin = Self::from_config(config)?;
        let _ = context.log("rixin_plugin_created");
        Ok(plugin)
    }

    fn invoke(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "provider.describe" => codey_plugin_sdk::serde_json::to_value(RouteDescriptor {
                name: "日日新".into(),
                base_url: BASE_URL.into(),
                upstream_protocol: "openaiResponses".into(),
                models: self.models.clone(),
                model_reasoning_efforts: Default::default(),
                headers: vec![],
                transport: None,
            })
            .map_err(|_| "无法生成日日新线路描述".into()),
            "request.beforeSend" => self.before_send(params),
            "request.afterHeaders" | "request.resume" => Ok(json!({"action":"continue"})),
            "request.completed" | "request.failed" | "request.cancelled" => Ok(json!({})),
            "ping" => Ok(json!({"plugin":"rixin", "keyCount":self.keys.len()})),
            _ => Err("不支持的插件方法".into()),
        }
    }
}

codey_plugin_sdk::export_plugin!(RixinPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn event(selected: bool) -> Value {
        json!({"stage":"beforeSend", "attempt":0, "metadata":{
            "apiKeyAuthorized":true, "apiKeySelected":selected,
            "officialAccount":false, "upstreamUrl":RESPONSE_URLS[0]
        }})
    }

    #[test]
    fn single_key_is_used_for_every_new_request() {
        let mut plugin = RixinPlugin::from_config(json!({"apiKey":"one"})).unwrap();
        for _ in 0..5 {
            assert_eq!(plugin.before_send(event(false)).unwrap()["apiKey"], "one");
        }
    }

    #[test]
    fn ordered_rotation_wraps_and_retry_does_not_advance() {
        let mut plugin = RixinPlugin::from_config(json!({"apiKeys":["a","b","c"]})).unwrap();
        for expected in ["a", "b", "c", "a", "b", "c", "a"] {
            assert_eq!(
                plugin.before_send(event(false)).unwrap()["apiKey"],
                expected
            );
            assert!(
                plugin
                    .before_send(event(true))
                    .unwrap()
                    .get("apiKey")
                    .is_none()
            );
        }
    }

    #[test]
    fn invalid_configuration_is_clear_and_never_echoes_keys() {
        for config in [
            json!(null),
            json!({}),
            json!({"apiKey":""}),
            json!({"apiKey":"  "}),
            json!({"apiKey":42}),
            json!({"apiKeys":[]}),
            json!({"apiKeys":"secret-value"}),
            json!({"apiKeys":["a",null]}),
            json!({"apiKeys":["secret-value\ninvalid"]}),
            json!({"apiKey":"secret-value", "apiKeys":["b"]}),
            json!({"apiKey":"a", "models":[]}),
            json!({"apiKey":"a", "models":["demo","DEMO"]}),
            json!({"apiKey":"a", "models":["codex-auto-review"]}),
            json!({"apiKey":"a", "models":["has space"]}),
            json!({"apiKey":"a", "unknown":"secret-value"}),
        ] {
            let error = RixinPlugin::from_config(config)
                .err()
                .expect("must reject invalid config");
            assert!(!error.is_empty());
            assert!(!error.contains("secret-value"));
        }
    }

    #[test]
    fn unrelated_routes_and_invalid_targets_do_not_advance() {
        let mut plugin = RixinPlugin::from_config(json!({"apiKeys":["a","b"]})).unwrap();
        let mut unauthorized = event(false);
        unauthorized["metadata"]["apiKeyAuthorized"] = json!(false);
        assert!(
            plugin
                .before_send(unauthorized)
                .unwrap()
                .get("apiKey")
                .is_none()
        );
        let mut wrong_target = event(false);
        wrong_target["metadata"]["upstreamUrl"] = json!("https://example.test/v1/responses");
        assert!(plugin.before_send(wrong_target).is_err());
        let mut missing = event(false);
        missing["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("apiKeySelected");
        assert!(plugin.before_send(missing).is_err());
        assert_eq!(plugin.before_send(event(false)).unwrap()["apiKey"], "a");
    }

    #[test]
    fn provider_registers_official_native_responses_without_secret_headers() {
        let mut plugin = RixinPlugin::from_config(json!({"apiKey":"a"})).unwrap();
        let descriptor = plugin.invoke("provider.describe", json!({})).unwrap();
        assert_eq!(descriptor["baseUrl"], BASE_URL);
        assert_eq!(descriptor["upstreamProtocol"], "openaiResponses");
        assert_eq!(descriptor["headers"], json!([]));
        assert!(plugin.invoke("unknown", json!({})).is_err());
    }

    #[test]
    fn concurrent_calls_follow_one_serial_cursor() {
        let plugin = Arc::new(Mutex::new((
            RixinPlugin::from_config(json!({"apiKeys":["a","b","c"]})).unwrap(),
            Vec::new(),
        )));
        let threads: Vec<_> = (0..12)
            .map(|_| {
                let shared = plugin.clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        let mut state = shared.lock().unwrap();
                        let chosen = state.0.before_send(event(false)).unwrap();
                        state.1.push(chosen["apiKey"].as_str().unwrap().to_owned());
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let state = plugin.lock().unwrap();
        assert_eq!(state.1.len(), 300);
        for (index, key) in state.1.iter().enumerate() {
            assert_eq!(key, ["a", "b", "c"][index % 3]);
        }
    }
}
