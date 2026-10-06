import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { loadTypeScriptModule } from "./helpers/load-typescript-module.mjs";

const shortNames = await loadTypeScriptModule(
  new URL("../src/routeShortNames.ts", import.meta.url),
);

test("third-party route short names are required and limited to four characters", () => {
  assert.equal(shortNames.MAX_ROUTE_SHORT_NAME_CHARACTERS, 4);
  assert.equal(shortNames.validateThirdPartyRouteShortName(""), "请输入短名称");
  assert.equal(shortNames.validateThirdPartyRouteShortName("中转"), "");
  assert.equal(shortNames.validateThirdPartyRouteShortName("中转线路"), "");
  assert.equal(
    shortNames.validateThirdPartyRouteShortName("中转线路名"),
    "短名称最多 4 个字符",
  );
  assert.equal(
    shortNames.validateThirdPartyRouteShortName("官"),
    "“官”仅供官方账号使用",
  );
  assert.equal(
    shortNames.validateThirdPartyRouteShortName(
      "中转",
      [{ id: "existing", authMode: "apiKey", officialAccount: false, shortName: "中转" }],
      "draft",
    ),
    "短名称“中转”已被其他线路使用",
  );
});

test("model labels use the default official prefix or a custom route short name", () => {
  const officialDefault = {
    authMode: "officialAccount",
    officialAccount: true,
    name: "OpenAI 官方直登",
    shortName: "",
  };
  const officialCustom = {
    authMode: "officialAccount",
    officialAccount: true,
    name: "OpenAI 官方直登",
    shortName: "官1",
  };
  const relay = {
    authMode: "apiKey",
    officialAccount: false,
    name: "备用中转线路",
    shortName: "备",
  };

  assert.equal(shortNames.prefixedRouteModelName(officialDefault, "gpt-5.6-sol"), "[官] gpt-5.6-sol");
  assert.equal(shortNames.prefixedRouteModelName(officialCustom, "gpt-5.6-sol"), "[官1] gpt-5.6-sol");
  assert.equal(shortNames.prefixedRouteModelName(relay, "claude-opus"), "[备] claude-opus");
  assert.equal(shortNames.fallbackRouteShortName(" 备用中转 "), "备用中转");
});

test("the third-party route editor exposes the route-name and short-name fields with maxLength", async () => {
  const source = await readFile(
    new URL("../src/ModelSection.tsx", import.meta.url),
    "utf8",
  );

  assert.match(source, /id="route-name-input"/);
  assert.match(source, /id="route-short-name-input"/);
  assert.match(source, /maxLength=\{MAX_ROUTE_NAME_CHARACTERS\}/);
  assert.match(source, /maxLength=\{MAX_ROUTE_SHORT_NAME_CHARACTERS\}/);
  assert.doesNotMatch(source, /最多 2 个字符且不可重复，模型名称前会显示为 \[短名称\]/);
  assert.match(
    source,
    /validateThirdPartyRouteShortName\(route\.shortName, profiles, route\.id\)/,
  );
});

test("official route short names may stay empty and only conflict with third-party routes", () => {
  const profiles = [
    {
      id: "official",
      authMode: "officialAccount",
      officialAccount: true,
      name: "官方线路",
      shortName: "官",
    },
    {
      id: "relay",
      authMode: "apiKey",
      officialAccount: false,
      name: "中转线路",
      shortName: "中转",
    },
  ];

  assert.equal(shortNames.validateOfficialRouteShortName(""), "");
  assert.equal(shortNames.validateOfficialRouteShortName("   "), "");
  assert.equal(shortNames.validateOfficialRouteShortName("自"), "");
  assert.equal(shortNames.validateOfficialRouteShortName("官方线路"), "");
  assert.equal(
    shortNames.validateOfficialRouteShortName("官方线路名"),
    "短名称最多 4 个字符",
  );
  assert.equal(
    shortNames.validateOfficialRouteShortName("中转", profiles),
    "短名称「中转」已被其他线路使用",
  );
});

test("official route settings are edited on the route card instead of the account row", async () => {
  const [modelSection, panel, app] = await Promise.all([
    readFile(new URL("../src/ModelSection.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/OfficialAccountsPanel.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/App.tsx", import.meta.url), "utf8"),
  ]);

  assert.match(modelSection, /id="official-route-name-input"/);
  assert.match(modelSection, /id="official-route-short-name-input"/);
  assert.match(modelSection, /id="official-route-base-url-input"/);
  assert.match(modelSection, /留空使用官方默认网关/);
  assert.match(modelSection, /maxLength=\{MAX_ROUTE_NAME_CHARACTERS\}/);
  assert.match(modelSection, /maxLength=\{MAX_ROUTE_SHORT_NAME_CHARACTERS\}/);
  assert.match(modelSection, /validateOfficialRouteSettings\(/);
  assert.match(modelSection, /官方账号登录 · \$\{email\}/);
  // 编辑按钮对所有线路渲染，只有第三方线路才有删除按钮。
  const editButton = modelSection.indexOf("aria-label={`编辑线路 ${profile.name}`}");
  const deleteGuard = modelSection.indexOf("{!isOfficial && (");
  assert.ok(editButton > 0 && deleteGuard > editButton);
  // 编辑只改线路名、短名称、网关和代理，模型列举交给同步入口。
  assert.match(modelSection, /openRouteDialog\(profile, isOfficial \? "settings" : null\)/);
  assert.match(modelSection, /const syncModels = \(\) => onFetchRouteModels\(profile\)/);
  assert.match(modelSection, /\{officialDialogScope !== "models" && \(/);
  assert.match(modelSection, /\{officialDialogScope !== "settings" && \(/);

  assert.match(panel, /aria-label=\{`移除官方账号 \$\{label\}`\}/);
  assert.doesNotMatch(panel, /IconPencil|validateOutboundProxyUrl/);

  assert.match(app, /accountId: routeSettings.accountId/);
  assert.match(app, /routeName: routeSettings.routeName/);
  assert.match(app, /routeShortName: routeSettings.routeShortName/);
  assert.match(app, /baseUrl: routeSettings.baseUrl/);
});

test("official accounts derive numbered default route names and short names", async () => {
  const [modelSection, mock] = await Promise.all([
    readFile(new URL("../src/ModelSection.tsx", import.meta.url), "utf8"),
    readFile(new URL("../src/dev/mockApi.ts", import.meta.url), "utf8"),
  ]);

  // 预览与后端一致：账号没有自定义名称时按添加顺序生成官方账号N / 官N。
  assert.match(mock, /const previewOfficialRouteName = \(index: number\) => `官方账号\$\{index\}`/);
  assert.match(mock, /if \(index <= 9\) return `官\$\{index\}`;/);
  assert.match(mock, /previewEnsureGeneratedRouteSettings\(\);/);
  assert.doesNotMatch(mock, /previewOfficialDerivedRouteName/);

  assert.doesNotMatch(modelSection, /留空则按账号添加顺序使用默认线路名，例如「官方账号1」/);
  assert.doesNotMatch(modelSection, /留空则按账号添加顺序使用默认短名称，例如「官1」/);
  assert.doesNotMatch(modelSection, /最多 2 个字符且不可重复，模型名称前会显示为 \[短名称\]/);
});

test("the route-name limit is shared by the renderer and the official account command", async () => {
  const [settings, backendConfig, officialAccounts] = await Promise.all([
    readFile(new URL("../src/officialRouteSettings.ts", import.meta.url), "utf8"),
    readFile(new URL("../backend/src/config.rs", import.meta.url), "utf8"),
    readFile(
      new URL("../backend/src/commands/official_accounts.rs", import.meta.url),
      "utf8",
    ),
  ]);

  assert.match(settings, /export const MAX_ROUTE_NAME_CHARACTERS = 15;/);
  assert.match(backendConfig, /pub const MAX_ROUTE_NAME_CHARS: usize = 15;/);
  // 后端保存线路时使用同一个上限，直接调用接口也写不进界面存不下的名称。
  assert.match(
    officialAccounts,
    /if route_name\.chars\(\)\.count\(\) > MAX_ROUTE_NAME_CHARS \{/,
  );
  assert.doesNotMatch(officialAccounts, /MAX_OFFICIAL_ROUTE_NAME_CHARS/);
});
