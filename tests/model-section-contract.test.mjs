import assert from "node:assert/strict";
import test from "node:test";

import { readSource } from "./helpers/read-source.mjs";

const [modelSectionSource, typeSource, dialogSource] = await Promise.all([
  readSource("src/ModelSection.tsx"),
  readSource("src/App.types.ts"),
  readSource("src/AppDialogs.tsx"),
]);

test("third-party native Web Search is an explicit Responses-only capability", () => {
  assert.match(typeSource, /supportsNativeWebSearch\?: boolean/);
  assert.match(modelSectionSource, /supportsNativeWebSearch: false/);
  assert.match(
    modelSectionSource,
    /upstreamProtocol === "openaiResponses"[\s\S]*?supportsNativeWebSearch:[\s\S]*?: false/,
  );
  assert.match(
    modelSectionSource,
    /checked=\{Boolean\(routeDraft\.supportsNativeWebSearch\)\}/,
  );
  assert.match(modelSectionSource, /aria-label="原生网页搜索"/);
});

test("official routes display WS while third-party routes remain explicit opt-in", () => {
  assert.match(modelSectionSource, /\(isOfficial \|\| profile\.supportsWebsockets\) &&/);
  assert.match(modelSectionSource, /routeDraft\.upstreamProtocol === "openaiResponses" && \(/);
  assert.match(modelSectionSource, /aria-label="WebSocket"/);
  assert.match(
    modelSectionSource,
    /checked=\{Boolean\(routeDraft\.supportsWebsockets\)\}[\s\S]*?disabled=\{isBusy\}/,
  );
  assert.doesNotMatch(
    modelSectionSource,
    /仅在线路明确支持 Responses WebSocket 时开启；连接失败会自动回退 HTTP\/SSE。/,
  );
});

test("official routes expose context budgets and preserve them when editing connection settings", async () => {
  const officialEditor = modelSectionSource
    .split('<div className="official-route-editor">')[1]
    .split('<div className="route-editor-form">')[0];
  assert.match(officialEditor, /ModelSettingsFields/);
  assert.match(modelSectionSource, /officialDialogScope === "models" \? officialModelContextDraft : undefined/);
  const backend = await readSource("backend/src/commands/models/defaults.rs");
  const saveOfficial = backend
    .split("pub async fn save_official_route_models(")[1]
    .split("pub(crate) async fn")[0];
  assert.match(saveOfficial, /set_model_contexts\(/);
  assert.doesNotMatch(saveOfficial, /set_model_reasoning_efforts\(/);
  const selection = await readSource("src/useModelSelection.ts");
  assert.match(selection, /"save_official_route_models", \{[\s\S]*?\.\.\.contextUpdate/);
  assert.match(dialogSource, /!routeConfigReadOnly && <ModelSettingsFields model=\{model.slug\}/);
});

test("model picker declares reasoning efforts and drops the 1M checkbox", () => {
  assert.doesNotMatch(dialogSource, /1M/);
  assert.match(dialogSource, /<ModelSettingsFields/);
  assert.match(dialogSource, /reasoningEffortAutoByModel\[key\]/);
  assert.match(dialogSource, /onUpdateDraftReasoningEffort\(model, efforts\)/);
});

test("model settings merge context budget and reasoning effort declarations", async () => {
  const [fieldsSource, presetsSource] = await Promise.all([
    readSource("src/components/ModelSettingsFields.tsx"),
    readSource("src/modelContextPresets.ts"),
  ]);
  const comboboxSource = await readSource("src/components/ModelContextWindowCombobox.tsx");
  assert.match(fieldsSource, /<ModelContextWindowCombobox/);
  assert.match(fieldsSource, /MODEL_REASONING_EFFORT_LEVELS/);
  assert.doesNotMatch(fieldsSource, /线上取值/);
  assert.match(comboboxSource, /allowsCustomValue/);
  assert.match(comboboxSource, /CONTEXT_WINDOW_PRESETS/);
  assert.match(presetsSource, /CONTEXT_WINDOW_PRESETS/);
  assert.match(presetsSource, /MAX_CONTEXT_WINDOW_TOKENS/);
});

test("official account panel is only mounted while the local router is on", () => {
  // 关闭本地路由时只读展示当前线路，账号登录区域不再出现；线路卡片仍需要
  // 账号列表显示所属邮箱，因此由线路面板自行读取一次。
  assert.match(
    modelSectionSource,
    /\{!routeConfigReadOnly && \(\s*<OfficialAccountsPanel/,
  );
  assert.match(
    modelSectionSource,
    /if \(!routeConfigReadOnly\) return;\s*void refreshOfficialAccounts\(\);/,
  );
});
