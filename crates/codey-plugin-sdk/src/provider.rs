//! 声明式线路；自定义传输须额外声明 transport 能力。
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CAPABILITY: &str = "provider.route.v1";
pub const METHOD_DESCRIBE: &str = "provider.describe";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteHeader {
    pub name: String,
    pub value: String,
}

/// `provider.describe` 的返回值。密钥由用户写在线路上，不在这里。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RouteDescriptor {
    pub name: String,
    pub base_url: String,
    pub upstream_protocol: String,
    pub models: Vec<String>,
    /// Thinking levels each model accepts. Keys must be listed in `models`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_reasoning_efforts: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub headers: Vec<RouteHeader>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<crate::transport::TransportOptions>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_rejects_unknown_fields() {
        let error = serde_json::from_value::<RouteDescriptor>(serde_json::json!({
            "name": "示例",
            "baseUrl": "https://example.test/v1",
            "upstreamProtocol": "openaiResponses",
            "models": ["demo"],
            "apiKey": "secret"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("apiKey"), "{error}");
    }

    #[test]
    fn descriptor_round_trips_model_reasoning_capabilities() {
        let descriptor = RouteDescriptor {
            name: "示例".into(),
            base_url: "https://example.test/v1".into(),
            upstream_protocol: "openaiResponses".into(),
            models: vec!["demo".into()],
            model_reasoning_efforts: std::collections::BTreeMap::from([(
                "demo".into(),
                vec!["low".into(), "xhigh".into()],
            )]),
            headers: Vec::new(),
            transport: None,
        };
        let value = serde_json::to_value(&descriptor).unwrap();
        assert_eq!(
            value["modelReasoningEfforts"]["demo"],
            serde_json::json!(["low", "xhigh"])
        );
        let decoded: RouteDescriptor = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, descriptor);
    }
}
