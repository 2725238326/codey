import assert from "node:assert/strict";
import test from "node:test";
import { autoStubModule, collectElements, createModuleGraph, elementProps } from "./helpers/jsx-tree.mjs";
import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const capability = ["low", "medium", "high", "xhigh"];
const template = [...capability, "max", "ultra"];
const efforts = (levels) => levels.map((level) => ({ level, value: level }));
const model = "gpt-6-astra";

test("插件能力独立于上游模板与用户勾选，旧高档位收敛且去重", async () => {
  const { resolveModelReasoningEfforts } = await loadTypeScriptModule(
    new URL("../src/modelReasoningEfforts.ts", import.meta.url),
  );
  const narrowed = resolveModelReasoningEfforts(template, efforts(["high"]), capability);
  assert.deepEqual(narrowed.autoEfforts, efforts(capability));
  assert.deepEqual(narrowed.efforts, efforts(["high"]));
  const migrated = resolveModelReasoningEfforts(template, [
    { level: "xhigh", value: "ultra" },
    ...efforts(["max", "ultra"]),
  ], capability);
  assert.deepEqual(migrated.efforts, efforts(["xhigh"]));
  assert.deepEqual(resolveModelReasoningEfforts(template, efforts(template), undefined).efforts, efforts(template));
  assert.deepEqual(resolveModelReasoningEfforts(template, efforts(template), template).efforts, efforts(template));
});

function picker({ plugin = true, declared = true, stored = efforts(["high"]) } = {}) {
  const config = {
    activeProfileId: "route-id",
    localRouterEnabled: true,
    profiles: [{
      id: "route-id", sourceProviderId: "different-provider-id", authMode: "apiKey",
      ...(plugin ? { pluginOwnerId: "dev.codey.excel-bridge" } : {}),
      ...(declared ? { pluginRouteSpec: { modelReasoningEfforts: { [model.toUpperCase()]: capability } } } : {}),
    }],
    selectedModelsByProvider: { "different-provider-id": [model] },
    modelReasoningEffortsByProvider: { "different-provider-id": { [model]: stored } },
  };
  const state = {
    officialModels: [], officialModelIds: [], thirdPartyModels: [model],
    manualThirdPartyModels: [], upstreamModels: [model], defaultModel: model,
    thirdPartyModelMetadata: [{ slug: model, autoSupportedReasoningEfforts: template }],
  };
  const graph = createModuleGraph(new URL("../src/useModelSelection.ts", import.meta.url), {
    stubs: { "./subagentModels": { buildSubagentModelOptions: () => [] } },
  });
  const render = () => {
    graph.restart();
    return graph.exports.useModelSelection({
      config, currentProvider: { id: "different-provider-id", official: false },
      officialAccountAvailable: false,
    });
  };
  render().openModelPicker(state);
  return render;
}

test("模型编辑器按所属线路提供四档，恢复自动适配不会保留用户缩小范围", () => {
  const render = picker();
  assert.deepEqual(render().modelPickerReasoningCapabilities, { [model]: capability });
  assert.deepEqual(render().reasoningEffortAutoByModel[model], efforts(capability));
  assert.deepEqual(render().draftReasoningEfforts[model], efforts(["high"]));
  render().resetDraftReasoningEffort(model);
  assert.deepEqual(render().draftReasoningEfforts[model], efforts(capability));
  const legacy = picker({ stored: efforts(["max", "ultra"]) });
  assert.deepEqual(legacy().draftReasoningEfforts[model], efforts(["xhigh"]));
});

test("普通线路和未声明能力的插件继续使用原有模板", () => {
  for (const options of [{ plugin: false }, { declared: false }]) {
    const render = picker({ ...options, stored: efforts(template) });
    assert.deepEqual(render().modelPickerReasoningCapabilities, {});
    assert.deepEqual(render().reasoningEffortAutoByModel[model], efforts(template));
    assert.deepEqual(render().draftReasoningEfforts[model], efforts(template));
  }
});

test("思考强度菜单仅展示明确允许的档位", () => {
  const graph = createModuleGraph(new URL("../src/components/ModelSettingsFields.tsx", import.meta.url), {
    stubs: {
      "@heroui/react": autoStubModule("heroui"),
      "@tabler/icons-react": autoStubModule("icons"),
      "./ui": autoStubModule("ui"),
    },
  });
  for (const supportedLevels of [capability, undefined]) {
    const tree = graph.exports.ModelSettingsFields({
      model, disabled: false, onChange() {},
      reasoning: { supportedLevels, autoEfforts: efforts(capability), efforts: efforts(["high"]), onChange() {}, onReset() {} },
    });
    const levels = collectElements(tree, (node) =>
      elementProps(node)?.["aria-label"]?.startsWith(`${model} 支持思考强度 `),
    ).map((node) => elementProps(node)["aria-label"].split(" ").at(-1));
    assert.deepEqual(levels, supportedLevels ?? template);
  }
});

test("子代理同名模型按各自线路限制档位，普通线路保留六档", () => {
  const graph = createModuleGraph(new URL("../src/subagentModels.ts", import.meta.url));
  const config = {
    localRouterEnabled: true,
    profiles: [
      { id: "ppt", name: "PPT", authMode: "apiKey", pluginOwnerId: "plugin", pluginRouteSpec: { modelReasoningEfforts: { [model]: capability } } },
      { id: "plain", name: "Plain", authMode: "apiKey" },
    ],
    selectedModelsByProvider: { ppt: [model], plain: [model] },
    declaredOfficialModelsByProvider: {},
  };
  const state = { officialModels: [], officialModelIds: [], thirdPartyModelMetadata: [{ slug: model, supportedReasoningEfforts: template, defaultReasoningEffort: "ultra" }] };
  const options = graph.exports.buildSubagentModelOptions(config, state, false);
  assert.deepEqual(options.find((item) => item.routeId === "ppt").supportedReasoningEfforts, capability);
  assert.deepEqual(options.find((item) => item.routeId === "plain").supportedReasoningEfforts, template);
  config.modelReasoningEffortsByProvider = { ppt: { [model]: efforts(["high"]) } };
  const narrowed = graph.exports.buildSubagentModelOptions(config, state, false);
  assert.deepEqual(narrowed.find((item) => item.routeId === "ppt").supportedReasoningEfforts, ["high"]);
});
