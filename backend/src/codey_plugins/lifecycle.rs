//! 可选的通用请求生命周期。原生回调超时只能停止等待，不能强制终止原生代码。
use super::{HeaderPatch, Manifest, Native, allowed_header_name, validate_patches};
use codey_plugin_sdk::lifecycle::{
    AUTH_CAPABILITY, Action, CAPABILITY, METHOD_AFTER_HEADERS, METHOD_BEFORE_SEND,
    METHOD_CANCELLED, METHOD_COMPLETED, METHOD_FAILED, METHOD_RESUME,
};
pub use codey_plugin_sdk::lifecycle::{Response as LifecycleResponse, Stage as LifecycleStage};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio::time::{Instant, sleep, timeout};

#[derive(Clone, Copy, Debug)]
pub enum LifecycleOutcome {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug)]
pub enum LifecycleDecision {
    Continue(Vec<HeaderPatch>),
    Retry(Vec<HeaderPatch>),
}

#[derive(Debug)]
pub struct LifecycleError {
    pub status: u16,
    pub code: String,
    pub message: String,
}
impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for LifecycleError {}
fn failure(code: &str) -> LifecycleError {
    LifecycleError {
        status: 502,
        code: code.into(),
        message: "插件请求生命周期处理失败".into(),
    }
}

type Callback = dyn Fn(&str, Value) -> Result<Value, String> + Send + Sync;
pub(super) struct LifecyclePlugin {
    id: String,
    callback: Arc<Callback>,
    active: AtomicBool,
    busy: Arc<AtomicBool>,
    waiters: Arc<Notify>,
    request_headers: Vec<String>,
    response_headers: Vec<String>,
    auth: bool,
    continue_on_failure: bool,
    max_wait: Duration,
    invoke_timeout: Duration,
}

impl LifecyclePlugin {
    pub(super) fn native(manifest: &Manifest, instance: Arc<Mutex<Native>>) -> Arc<Self> {
        Arc::new(Self {
            id: manifest.id.clone(),
            callback: Arc::new(move |method, params| {
                instance
                    .try_lock()
                    .map_err(|_| "插件实例忙或锁已损坏".to_owned())?
                    .invoke(method, params)
            }),
            active: AtomicBool::new(false),
            busy: Arc::new(AtomicBool::new(false)),
            waiters: Arc::new(Notify::new()),
            request_headers: manifest.header_names.clone(),
            response_headers: manifest.response_header_names.clone(),
            auth: manifest.capabilities.iter().any(|s| s == AUTH_CAPABILITY),
            continue_on_failure: manifest.lifecycle_failure_policy.as_deref() == Some("continue"),
            max_wait: Duration::from_millis(manifest.lifecycle_max_wait_ms.unwrap_or(30_000)),
            invoke_timeout: Duration::from_secs(3),
        })
    }

    pub(super) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
        self.waiters.notify_waiters();
    }

    pub(super) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(super) async fn call(
        self: &Arc<Self>,
        method: &'static str,
        params: Value,
        deadline: Option<Instant>,
    ) -> Result<Value, LifecycleError> {
        if !self.active.load(Ordering::Acquire) && method != codey_plugin_sdk::transport::STOP {
            return Err(failure("plugin_disabled"));
        }
        let call_deadline = (Instant::now() + self.invoke_timeout)
            .min(deadline.unwrap_or_else(|| Instant::now() + self.invoke_timeout));
        // 正常并发在异步任务中排队，只有拿到实例执行权才创建阻塞任务。
        // 排队和原生调用共用一次回调期限，挂起实例不会积累阻塞线程。
        // 先订阅 Notify 再 CAS，避免实例刚释放时丢掉唤醒。
        let guard = loop {
            if !self.active.load(Ordering::Acquire) && method != codey_plugin_sdk::transport::STOP {
                return Err(failure("plugin_disabled"));
            }
            let remaining = call_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(failure("plugin_busy"));
            }
            let notified = self.waiters.notified();
            if let Some(guard) = BusyGuard::acquire(self.busy.clone(), self.waiters.clone()) {
                break guard;
            }
            tokio::select! {
                _ = sleep(remaining) => return Err(failure("plugin_busy")),
                _ = notified => {}
            }
        };
        let plugin = self.clone();
        let mut task = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            if !plugin.active.load(Ordering::Acquire) && method != codey_plugin_sdk::transport::STOP
            {
                return Err("插件已停用".into());
            }
            (plugin.callback)(method, params)
        });
        let duration = call_deadline.saturating_duration_since(Instant::now());
        // 原生超时只能停止等待，不能杀掉阻塞回调。停用探测必须和实例 Notify 分开：
        // 持有 BusyGuard 时不能再等同一把锁的唤醒，否则会抢走排队请求的许可。
        let result = timeout(duration, async {
            loop {
                tokio::select! {
                    result = &mut task => return result.map_err(|_| failure("plugin_callback_failed"))?
                        .map_err(|error| failure(if codey_plugin_sdk::transport::is_reserved(method) {
                            codey_plugin_sdk::transport::public_error_code(&error)
                        } else { "plugin_callback_failed" })),
                    _ = sleep(Duration::from_millis(50)) => {
                        if !self.active.load(Ordering::Acquire) && method != codey_plugin_sdk::transport::STOP { return Err(failure("plugin_disabled")); }
                    }
                }
            }
        }).await.map_err(|_| failure("plugin_callback_timeout"))?;
        if !self.active.load(Ordering::Acquire) && method != codey_plugin_sdk::transport::STOP {
            return Err(failure("plugin_disabled"));
        }
        result
    }
}

struct BusyGuard {
    busy: Arc<AtomicBool>,
    waiters: Arc<Notify>,
}
impl BusyGuard {
    fn acquire(busy: Arc<AtomicBool>, waiters: Arc<Notify>) -> Option<Self> {
        busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self { busy, waiters })
    }
}
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
        self.waiters.notify_one();
    }
}

static PLUGINS: OnceLock<arc_swap::ArcSwap<Vec<Arc<LifecyclePlugin>>>> = OnceLock::new();
fn plugins() -> &'static arc_swap::ArcSwap<Vec<Arc<LifecyclePlugin>>> {
    PLUGINS.get_or_init(|| arc_swap::ArcSwap::from_pointee(Vec::new()))
}

/// 与 [`LifecycleRequest::new`] 同一快照源：测试优先 `TEST_PLUGINS`，否则 ArcSwap。
/// 空检查用 `load()` 而不是 `load_full()`，避免无插件热路径上的 Arc clone。
pub(crate) fn has_plugins() -> bool {
    #[cfg(test)]
    if let Ok(active) = TEST_PLUGINS.try_with(|plugins| !plugins.is_empty()) {
        return active;
    }
    !plugins().load().is_empty()
}
pub(super) fn enabled(manifest: &Manifest) -> bool {
    manifest.capabilities.iter().any(|s| s == CAPABILITY)
}
pub(super) fn publish(mut next: Vec<Arc<LifecyclePlugin>>) {
    next.sort_by(|a, b| a.id.cmp(&b.id));
    let previous = plugins().load_full();
    for old in previous.iter() {
        if !next.iter().any(|new| Arc::ptr_eq(old, new)) {
            old.active.store(false, Ordering::Release);
            old.waiters.notify_waiters();
        }
    }
    for entry in &next {
        entry.active.store(true, Ordering::Release);
    }
    plugins().store(Arc::new(next));
}

pub(super) fn allowed_response_header_name(name: &str) -> bool {
    // Response transport metadata is read-only; credential headers remain private.
    allowed_header_name(name)
        || [
            "content-type",
            "content-length",
            "content-encoding",
            "retry-after",
        ]
        .iter()
        .any(|allowed| name.eq_ignore_ascii_case(allowed))
}

struct EntryState {
    plugin: Arc<LifecyclePlugin>,
    context: Option<Value>,
}
pub struct LifecycleRequest {
    metadata: Value,
    credentials: Option<Value>,
    entries: Vec<EntryState>,
    finished: bool,
    remaining_wait: Duration,
}

impl LifecycleRequest {
    pub fn new(metadata: Value, credentials: Option<Value>) -> Self {
        #[cfg(test)]
        let snapshot = TEST_PLUGINS
            .try_with(Clone::clone)
            .unwrap_or_else(|_| plugins().load_full());
        #[cfg(not(test))]
        let snapshot = plugins().load_full();
        Self {
            metadata,
            credentials,
            entries: snapshot
                .iter()
                .map(|plugin| EntryState {
                    plugin: plugin.clone(),
                    context: None,
                })
                .collect(),
            finished: false,
            remaining_wait: Duration::from_secs(600),
        }
    }

    pub(crate) fn inert() -> Self {
        Self {
            metadata: Value::Null,
            credentials: None,
            entries: Vec::new(),
            finished: false,
            remaining_wait: Duration::from_secs(600),
        }
    }
    pub fn is_active(&self) -> bool {
        !self.finished && !self.entries.is_empty()
    }

    pub async fn dispatch(
        &mut self,
        stage: LifecycleStage,
        attempt: u32,
        headers: BTreeMap<String, String>,
        response: Option<LifecycleResponse>,
    ) -> Result<LifecycleDecision, LifecycleError> {
        if self.finished {
            return Err(failure("plugin_request_finished"));
        }
        let mut visible: BTreeMap<String, String> = headers
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect();
        let mut patches = Vec::new();
        for entry in &mut self.entries {
            let plugin = entry.plugin.clone();
            let selected = selected_headers(&visible, &plugin.request_headers);
            let selected_response = response.as_ref().map(|response| {
                json!({"status":response.status,
                "headers":selected_headers(&response.headers, &plugin.response_headers)})
            });
            let mut params = json!({"metadata":self.metadata,"requestId":self.metadata.get("requestId"),
                "stage":stage,"attempt":attempt,"headers":selected,"response":selected_response});
            if plugin.auth
                && let Some(credentials) = &self.credentials
            {
                params["credentials"] = credentials.clone();
            }
            entry.context = Some(params);
            if self.remaining_wait.is_zero() {
                return Err(failure("plugin_request_wait_timeout"));
            }
            let started = Instant::now();
            let result = dispatch_one(entry, stage, started + self.remaining_wait).await;
            self.remaining_wait = self.remaining_wait.saturating_sub(started.elapsed());
            let action = match result {
                Ok(action) => action,
                Err(_) if plugin.continue_on_failure => continue,
                Err(error) => return Err(error),
            };
            match action {
                ParsedAction::Continue(next) => {
                    for patch in &next {
                        match &patch.value {
                            Some(value) => {
                                visible.insert(patch.name.clone(), value.clone());
                            }
                            None => {
                                visible.remove(&patch.name);
                            }
                        }
                    }
                    patches.extend(next);
                }
                ParsedAction::Retry(next) => {
                    patches.extend(next);
                    return Ok(LifecycleDecision::Retry(patches));
                }
                ParsedAction::Abort(error) => return Err(error),
                ParsedAction::Wait(_, _) => unreachable!("dispatch_one resolves waits"),
            }
        }
        Ok(LifecycleDecision::Continue(patches))
    }

    /// 终态通知不可改变请求结果，也不会等待原生回调或实例销毁。
    pub fn finish(&mut self, outcome: LifecycleOutcome, status: Option<u16>, code: Option<&str>) {
        if self.finished {
            return;
        }
        self.finished = true;
        let method = match outcome {
            LifecycleOutcome::Completed => METHOD_COMPLETED,
            LifecycleOutcome::Failed => METHOD_FAILED,
            LifecycleOutcome::Cancelled => METHOD_CANCELLED,
        };
        for entry in std::mem::take(&mut self.entries) {
            let metadata = self.metadata.clone();
            let code = code.map(|code| code.chars().take(128).collect::<String>());
            // Each plugin gets an independent best-effort notification. Destruction
            // of retired native instances also stays off the request thread.
            let notify = move || {
                let mut params = entry.context.unwrap_or_else(|| json!({"metadata":metadata,"requestId":metadata.get("requestId"),"attempt":0,"stage":"beforeSend"}));
                // Terminal messages carry no credentials or request/response header values.
                params.as_object_mut().unwrap().remove("credentials");
                params.as_object_mut().unwrap().remove("headers");
                params.as_object_mut().unwrap().remove("response");
                params["status"] = json!(status);
                params["code"] = json!(code);
                if let Some(_guard) =
                    BusyGuard::acquire(entry.plugin.busy.clone(), entry.plugin.waiters.clone())
                {
                    let _ = (entry.plugin.callback)(method, params);
                }
            };
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(notify);
            } else {
                let _ = std::thread::Builder::new()
                    .name("plugin-cleanup".into())
                    .spawn(notify);
            }
        }
    }
}
impl Drop for LifecycleRequest {
    fn drop(&mut self) {
        self.finish(LifecycleOutcome::Cancelled, None, None);
    }
}

fn selected_headers(
    headers: &BTreeMap<String, String>,
    names: &[String],
) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter(|(key, _)| names.iter().any(|name| name.eq_ignore_ascii_case(key)))
        .map(|(key, value)| (key.to_ascii_lowercase(), value.clone()))
        .collect()
}

enum ParsedAction {
    Continue(Vec<HeaderPatch>),
    Retry(Vec<HeaderPatch>),
    Wait(String, Duration),
    Abort(LifecycleError),
}
fn parse_action(
    value: Value,
    stage: LifecycleStage,
    headers: &[String],
) -> Result<ParsedAction, LifecycleError> {
    let action: Action =
        serde_json::from_value(value).map_err(|_| failure("plugin_invalid_action"))?;
    match action {
        Action::Continue { headers: patches } => {
            parse_header_action(patches, stage, headers, false)
        }
        Action::Retry { headers: patches } => parse_header_action(patches, stage, headers, true),
        Action::Wait {
            token,
            poll_after_ms,
        } => {
            if token.is_empty() || token.len() > 256 || token.chars().any(char::is_control) {
                return Err(failure("plugin_invalid_action"));
            }
            Ok(ParsedAction::Wait(
                token,
                Duration::from_millis(poll_after_ms.unwrap_or(100).clamp(50, 1000)),
            ))
        }
        Action::Abort {
            status,
            code,
            message,
        } => {
            let status = status.unwrap_or(502);
            let code = code.unwrap_or_else(|| "plugin_aborted".into());
            let message = message.unwrap_or_else(|| "插件终止了请求".into());
            if !(400..=599).contains(&status)
                || code.is_empty()
                || code.len() > 128
                || message.len() > 2048
                || code.chars().any(char::is_control)
                || message.chars().any(char::is_control)
            {
                return Err(failure("plugin_invalid_action"));
            }
            Ok(ParsedAction::Abort(LifecycleError {
                status,
                code,
                message,
            }))
        }
    }
}
fn parse_header_action(
    patches: Vec<codey_plugin_sdk::lifecycle::HeaderPatch>,
    stage: LifecycleStage,
    headers: &[String],
    retry: bool,
) -> Result<ParsedAction, LifecycleError> {
    if retry && stage == LifecycleStage::BeforeSend
        || !retry && stage == LifecycleStage::AfterHeaders && !patches.is_empty()
    {
        return Err(failure("plugin_invalid_action"));
    }
    let patches = validate_patches(json!({"headers":patches}), headers)
        .map_err(|_| failure("plugin_invalid_headers"))?;
    Ok(if retry {
        ParsedAction::Retry(patches)
    } else {
        ParsedAction::Continue(patches)
    })
}

async fn dispatch_one(
    entry: &mut EntryState,
    stage: LifecycleStage,
    overall_deadline: Instant,
) -> Result<ParsedAction, LifecycleError> {
    let plugin = entry.plugin.clone();
    let mut method = match stage {
        LifecycleStage::BeforeSend => METHOD_BEFORE_SEND,
        LifecycleStage::AfterHeaders => METHOD_AFTER_HEADERS,
    };
    let mut deadline = None;
    let mut token: Option<String> = None;
    loop {
        let call_deadline = deadline.unwrap_or(overall_deadline).min(overall_deadline);
        let value = match plugin
            .call(
                method,
                entry.context.as_ref().unwrap().clone(),
                Some(call_deadline),
            )
            .await
        {
            Ok(value) => value,
            // 续发排队和回调共用等待期限。期限正好在这次调用里耗尽时，对外的
            // 原因仍是等待超时，而不是实例忙或回调慢；停用是另一种状态，保留。
            Err(error)
                if error.code != "plugin_disabled"
                    && deadline.is_some_and(|end| Instant::now() >= end) =>
            {
                return Err(failure("plugin_wait_timeout"));
            }
            Err(error) => return Err(error),
        };
        let action = parse_action(value, stage, &plugin.request_headers)?;
        match action {
            ParsedAction::Wait(next_token, delay) => {
                if token.as_ref().is_some_and(|token| token != &next_token) {
                    return Err(failure("plugin_invalid_token"));
                }
                token = Some(next_token.clone());
                entry.context.as_mut().unwrap()["token"] = json!(next_token);
                let end = *deadline.get_or_insert_with(|| {
                    (Instant::now() + plugin.max_wait).min(overall_deadline)
                });
                let wake = Instant::now() + delay;
                while Instant::now() < wake {
                    let notified = plugin.waiters.notified();
                    if !plugin.active.load(Ordering::Acquire)
                        && method != codey_plugin_sdk::transport::STOP
                    {
                        return Err(failure("plugin_disabled"));
                    }
                    if Instant::now() >= end {
                        return Err(failure("plugin_wait_timeout"));
                    }
                    let remaining = Duration::from_millis(50)
                        .min(wake.saturating_duration_since(Instant::now()))
                        .min(end.saturating_duration_since(Instant::now()));
                    if remaining.is_zero() {
                        break;
                    }
                    tokio::select! {
                        _ = sleep(remaining) => {}
                        _ = notified => {}
                    }
                }
                if Instant::now() >= end {
                    return Err(failure("plugin_wait_timeout"));
                }
                method = METHOD_RESUME;
            }
            other => return Ok(other),
        }
    }
}

#[cfg(test)]
tokio::task_local! { static TEST_PLUGINS: Arc<Vec<Arc<LifecyclePlugin>>>; }
#[cfg(test)]
pub(crate) struct TestPlugin {
    pub(super) plugin: Arc<LifecyclePlugin>,
}
#[cfg(test)]
impl TestPlugin {
    pub(crate) fn new(
        id: &str,
        callback: impl Fn(&str, Value) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            plugin: Arc::new(LifecyclePlugin {
                id: id.into(),
                callback: Arc::new(callback),
                active: AtomicBool::new(true),
                busy: Arc::new(AtomicBool::new(false)),
                waiters: Arc::new(Notify::new()),
                request_headers: vec!["x-test".into()],
                response_headers: vec!["x-test".into(), "content-type".into()],
                auth: false,
                continue_on_failure: false,
                max_wait: Duration::from_millis(300),
                invoke_timeout: Duration::from_millis(100),
            }),
        }
    }
}
#[cfg(test)]
pub(crate) async fn with_test_plugins<F: std::future::Future>(
    entries: Vec<TestPlugin>,
    future: F,
) -> F::Output {
    let mut entries: Vec<_> = entries.into_iter().map(|entry| entry.plugin).collect();
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    TEST_PLUGINS.scope(Arc::new(entries), future).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> LifecycleRequest {
        LifecycleRequest::new(
            json!({"requestId":"test-request","model":"any-future-model"}),
            Some(json!({"secret":"not-visible"})),
        )
    }
    fn before() -> LifecycleStage {
        LifecycleStage::BeforeSend
    }
    fn after() -> LifecycleStage {
        LifecycleStage::AfterHeaders
    }
    fn headers() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("x-test".into(), "original".into()),
            ("authorization".into(), "private".into()),
        ])
    }

    #[tokio::test]
    async fn empty_snapshot_is_inactive_without_credentials() {
        with_test_plugins(vec![], async {
            assert!(!has_plugins());
            let request = LifecycleRequest::inert();
            assert!(!request.is_active());
            assert!(request.metadata.is_null());
            assert!(request.credentials.is_none());
        })
        .await;
    }

    #[tokio::test]
    async fn has_plugins_reads_the_same_snapshot_as_new() {
        with_test_plugins(
            vec![TestPlugin::new("a", |_, _| {
                Ok(json!({"action": "continue"}))
            })],
            async {
                assert!(has_plugins());
                assert!(LifecycleRequest::new(json!({}), None).is_active());
            },
        )
        .await;
        with_test_plugins(vec![], async {
            assert!(!has_plugins());
            assert!(!LifecycleRequest::new(json!({}), None).is_active());
        })
        .await;
    }

    #[test]
    fn action_validation_is_strict_and_stage_aware() {
        let allowed = vec!["x-test".into()];
        for value in [
            json!({"action":"unknown"}),
            json!({"action":"continue","extra":true}),
            json!({"action":"wait","token":"x","readyAction":"continue"}),
            json!({"action":"wait","token":"x".repeat(257)}),
            json!({"action":"abort","status":200}),
            json!({"action":"abort","code":"x".repeat(129)}),
            json!({"action":"abort","message":"x".repeat(2049)}),
        ] {
            assert!(parse_action(value, before(), &allowed).is_err());
        }
        assert!(parse_action(json!({"action":"retry"}), before(), &allowed).is_err());
        assert!(matches!(
            parse_action(json!({"action":"retry"}), after(), &allowed).unwrap(),
            ParsedAction::Retry(_)
        ));
        assert!(
            parse_action(
                json!({"action":"continue","headers":[{"name":"x-test","value":"x"}]}),
                after(),
                &allowed
            )
            .is_err()
        );
        for name in ["authorization", "cookie", "x-other"] {
            assert!(
                parse_action(
                    json!({"action":"retry","headers":[{"name":name,"value":"x"}]}),
                    after(),
                    &allowed
                )
                .is_err()
            );
        }
        for (input, expected) in [(0, 50), (u64::MAX, 1000)] {
            match parse_action(
                json!({"action":"wait","token":"t","pollAfterMs":input}),
                before(),
                &allowed,
            )
            .unwrap()
            {
                ParsedAction::Wait(_, delay) => assert_eq!(delay.as_millis(), expected),
                _ => panic!(),
            }
        }
    }

    #[tokio::test]
    async fn plugins_run_in_id_order_with_filtered_context_and_prior_patches() {
        let first = TestPlugin::new("a", |method, params| {
            if method == "request.beforeSend" {
                assert_eq!(params["headers"], json!({"x-test":"original"}));
                assert!(params.get("credentials").is_none());
                assert_eq!(params["metadata"]["model"], "any-future-model");
                return Ok(
                    json!({"action":"continue","headers":[{"name":"X-Test","value":"patched"}]}),
                );
            }
            Ok(json!({"action":"continue"}))
        });
        let second = TestPlugin::new("b", |method, params| {
            if method == "request.beforeSend" {
                assert_eq!(params["headers"]["x-test"], "patched");
            }
            if method == "request.afterHeaders" {
                assert_eq!(
                    params["response"]["headers"],
                    json!({"content-type":"text/event-stream"})
                );
            }
            Ok(json!({"action":"continue"}))
        });
        with_test_plugins(vec![second,first],async {
            let mut request = request();
            assert!(request.is_active());
            let decision = request.dispatch(before(),0,headers(),None).await.unwrap();
            assert!(matches!(decision,LifecycleDecision::Continue(ref patches) if patches.len()==1 && patches[0].value.as_deref()==Some("patched")));
            let response = LifecycleResponse { status:200,headers:BTreeMap::from([("Content-Type".into(),"text/event-stream".into()),("set-cookie".into(),"private".into())]) };
            request.dispatch(after(),0,headers(),Some(response)).await.unwrap();
            request.finish(LifecycleOutcome::Completed,Some(200),None);
            assert!(!request.is_active());
        }).await;
    }

    #[tokio::test]
    async fn auth_capability_receives_credentials_and_wait_resumes_original_stage() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut plugin = TestPlugin::new("a", move |method, params| {
            tx.send((method.to_owned(), params.clone())).unwrap();
            Ok(match method {
                "request.afterHeaders" => json!({"action":"wait","token":"job","pollAfterMs":1}),
                "request.resume" => {
                    json!({"action":"retry","headers":[{"name":"x-test","value":"ready"}]})
                }
                _ => json!({"action":"continue"}),
            })
        });
        Arc::get_mut(&mut plugin.plugin).unwrap().auth = true;
        with_test_plugins(vec![plugin],async {
            let mut request = request();
            let decision = request.dispatch(after(),0,headers(),Some(LifecycleResponse { status:200,headers:BTreeMap::new() })).await.unwrap();
            assert!(matches!(decision,LifecycleDecision::Retry(ref patches) if patches[0].value.as_deref()==Some("ready")));
            let (_,initial) = rx.recv().await.unwrap();
            let (method,resumed) = rx.recv().await.unwrap();
            assert_eq!(initial["credentials"]["secret"],"not-visible");
            assert_eq!(method,"request.resume");
            assert_eq!(resumed["stage"],"afterHeaders");
            assert_eq!(resumed["token"],"job");
            assert_eq!(resumed["attempt"],0);
            request.finish(LifecycleOutcome::Completed,Some(200),None);
            request.finish(LifecycleOutcome::Failed,Some(500),Some("ignored"));
            let (method,terminal) = timeout(Duration::from_secs(1),rx.recv()).await.unwrap().unwrap();
            assert_eq!(method,"request.completed");
            assert_eq!(terminal["token"],"job");
            assert!(terminal.get("credentials").is_none());
            assert!(rx.try_recv().is_err());
        }).await;
    }

    #[tokio::test]
    async fn token_changes_and_fixed_wait_deadlines_are_rejected() {
        // 续发换令牌要被拒；等待期限耗尽时，无论是轮询排队还是回调自己用光
        // 期限，对外都只报等待超时。
        for case in ["change_token", "poll_until_deadline", "slow_resume"] {
            let mut plugin = TestPlugin::new("a", move |method, _| {
                if method == "request.resume" {
                    match case {
                        "change_token" => {
                            return Ok(json!({"action":"wait","token":"other","pollAfterMs":50}));
                        }
                        "slow_resume" => std::thread::sleep(Duration::from_millis(200)),
                        _ => {}
                    }
                }
                Ok(json!({"action":"wait","token":"same","pollAfterMs":50}))
            });
            Arc::get_mut(&mut plugin.plugin).unwrap().max_wait = Duration::from_millis(120);
            with_test_plugins(vec![plugin], async {
                let mut request = request();
                let error = request
                    .dispatch(before(), 0, headers(), None)
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code,
                    if case == "change_token" {
                        "plugin_invalid_token"
                    } else {
                        "plugin_wait_timeout"
                    }
                );
                request.finish(LifecycleOutcome::Failed, None, Some(&error.code));
            })
            .await;
        }
    }

    #[tokio::test]
    async fn drop_notifies_cancelled_with_the_pending_token() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let plugin = TestPlugin::new("a", move |method, params| {
            if method == "request.cancelled" {
                tx.send(params).unwrap();
            }
            Ok(json!({"action":"wait","token":"pending","pollAfterMs":1000}))
        });
        with_test_plugins(vec![plugin], async {
            let mut request = request();
            assert!(
                timeout(
                    Duration::from_millis(30),
                    request.dispatch(before(), 0, headers(), None)
                )
                .await
                .is_err()
            );
            drop(request);
            let params = timeout(Duration::from_secs(1), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(params["token"], "pending");
        })
        .await;
    }

    #[tokio::test]
    async fn failed_callback_policy_and_retired_generation_are_independent() {
        for continue_on_failure in [false, true] {
            let mut old = TestPlugin::new("same-id", |_, _| Ok(json!({"action":"continue"})));
            Arc::get_mut(&mut old.plugin).unwrap().continue_on_failure = continue_on_failure;
            let old_instance = old.plugin.clone();
            with_test_plugins(vec![old], async {
                let mut old_request = request();
                old_instance.active.store(false, Ordering::Release);
                let replacement =
                    TestPlugin::new("same-id", |_, _| Ok(json!({"action":"continue"})));
                with_test_plugins(vec![replacement], async {
                    let result = old_request.dispatch(before(), 0, headers(), None).await;
                    assert_eq!(result.is_ok(), continue_on_failure);
                    let mut fresh = request();
                    assert!(fresh.dispatch(before(), 0, headers(), None).await.is_ok());
                    fresh.finish(LifecycleOutcome::Completed, Some(200), None);
                })
                .await;
                old_request.finish(LifecycleOutcome::Failed, None, None);
            })
            .await;
        }
    }

    #[tokio::test]
    async fn concurrent_requests_wait_for_a_healthy_instance_without_failing() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let mut plugin = TestPlugin::new("a", move |method, _| {
            if method == "request.beforeSend" {
                counter.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(json!({"action":"continue"}))
        });
        Arc::get_mut(&mut plugin.plugin).unwrap().invoke_timeout = Duration::from_secs(1);
        with_test_plugins(vec![plugin], async {
            let mut first = LifecycleRequest::new(json!({"requestId":"first"}), None);
            let mut second = LifecycleRequest::new(json!({"requestId":"second"}), None);
            let (first_result, second_result) = tokio::join!(
                first.dispatch(before(), 0, headers(), None),
                second.dispatch(before(), 0, headers(), None),
            );
            assert!(first_result.is_ok());
            assert!(second_result.is_ok());
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            first.finish(LifecycleOutcome::Completed, Some(200), None);
            second.finish(LifecycleOutcome::Completed, Some(200), None);
        })
        .await;
    }

    #[tokio::test]
    async fn timeout_keeps_the_instance_busy_until_native_callback_returns() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let mut plugin = TestPlugin::new("a", move |method, _| {
            if method == "request.beforeSend" {
                counter.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(140));
            }
            Ok(json!({"action":"continue"}))
        });
        Arc::get_mut(&mut plugin.plugin).unwrap().invoke_timeout = Duration::from_millis(20);
        with_test_plugins(vec![plugin], async {
            let mut first = request();
            assert_eq!(
                first
                    .dispatch(before(), 0, headers(), None)
                    .await
                    .unwrap_err()
                    .code,
                "plugin_callback_timeout"
            );
            let mut second = request();
            assert_eq!(
                second
                    .dispatch(before(), 0, headers(), None)
                    .await
                    .unwrap_err()
                    .code,
                "plugin_busy"
            );
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            first.finish(LifecycleOutcome::Failed, None, None);
            second.finish(LifecycleOutcome::Failed, None, None);
            sleep(Duration::from_millis(160)).await;
        })
        .await;
    }

    #[tokio::test]
    async fn failure_policy_does_not_override_an_explicit_abort_or_expose_native_errors() {
        for abort in [false, true] {
            let mut plugin = TestPlugin::new("a", move |_, _| {
                if abort {
                    Ok(
                        json!({"action":"abort","status":429,"code":"policy_denied","message":"暂停请求"}),
                    )
                } else {
                    Err("secret credential in native error".into())
                }
            });
            Arc::get_mut(&mut plugin.plugin)
                .unwrap()
                .continue_on_failure = abort;
            with_test_plugins(vec![plugin], async {
                let mut request = request();
                let error = request
                    .dispatch(before(), 0, headers(), None)
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code,
                    if abort {
                        "policy_denied"
                    } else {
                        "plugin_callback_failed"
                    }
                );
                assert!(!error.message.contains("credential"));
                request.finish(
                    LifecycleOutcome::Failed,
                    Some(error.status),
                    Some(&error.code),
                );
            })
            .await;
        }
    }

    #[tokio::test]
    async fn disabling_a_waiting_instance_and_cumulative_deadline_stop_resume() {
        for disable in [true, false] {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let plugin = TestPlugin::new("a", move |method, _| {
                if method == "request.beforeSend" {
                    let _ = tx.send(());
                }
                Ok(json!({"action":"wait","token":"job","pollAfterMs":1000}))
            });
            let instance = plugin.plugin.clone();
            with_test_plugins(vec![plugin], async {
                let mut request = request();
                request.remaining_wait = Duration::from_millis(100);
                let disable_instance = async {
                    rx.recv().await.unwrap();
                    if disable {
                        instance.active.store(false, Ordering::Release);
                    }
                };
                let (result, _) = tokio::join!(
                    request.dispatch(before(), 0, headers(), None),
                    disable_instance
                );
                assert_eq!(
                    result.unwrap_err().code,
                    if disable {
                        "plugin_disabled"
                    } else {
                        "plugin_wait_timeout"
                    }
                );
                request.finish(LifecycleOutcome::Cancelled, None, None);
            })
            .await;
        }
    }
}
