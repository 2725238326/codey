import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { assertSameAssets, callback, identity, main, signature, validateBuild, validateNotes, validateRelease } from "../scripts/release-automation.mjs";

const environment = {
  RELEASE_BUILD_ID: "build_test-123", RELEASE_ATTEMPT: "2", GITHUB_RUN_ID: "456", RELEASE_ACTION: "build",
  RELEASE_SOURCE_SHA: "a".repeat(40), RELEASE_VERSION: "1.2.3", RELEASE_BASE_SHA: "b".repeat(40), RELEASE_BASE_TAG: "v1.2.2",
  GITHUB_REPOSITORY: "owner/codey", CODEY_RELEASE_ADMIN_URL: "https://admin.example.com", RELEASE_ADMIN_CALLBACK_SECRET: "s".repeat(40),
};
const build = {
  id: environment.RELEASE_BUILD_ID, repository: environment.GITHUB_REPOSITORY, version: "1.2.3", tag: "v1.2.3",
  source_sha: environment.RELEASE_SOURCE_SHA, base_sha: environment.RELEASE_BASE_SHA, base_tag: "v1.2.2", action: "build", attempt: 2,
};

test("callback signs the exact body and includes immutable execution identity", async () => {
  let calls = 0;
  const result = await callback("events", { status: "succeeded", attempt: 99 }, environment, async (url, options) => {
    calls += 1;
    assert.equal(url, "https://admin.example.com/api/internal/builds/build_test-123/events");
    const body = JSON.parse(options.body);
    assert.deepEqual(body, { status: "succeeded", attempt: 2, run_id: 456, action: "build", source_sha: "a".repeat(40) });
    assert.equal(options.headers["x-release-signature"], createHmac("sha256", environment.RELEASE_ADMIN_CALLBACK_SECRET).update(`${options.headers["x-release-timestamp"]}.${options.body}`).digest("hex"));
    assert.equal(options.redirect, "error");
    return new Response('{"ok":true}');
  });
  assert.equal(calls, 1);
  assert.deepEqual(result, { ok: true });
  assert.equal(signature("123", "{}", "secret").length, 64);
});

test("invalid identity and insecure callback endpoints never reach the network", async () => {
  assert.throws(() => identity({ ...environment, RELEASE_SOURCE_SHA: "master" }), /身份参数/);
  assert.throws(() => identity({ ...environment, RELEASE_ATTEMPT: "1.5" }), /身份参数/);
  assert.throws(() => identity({ ...environment, RELEASE_ACTION: "publish" }), /身份参数/);
  const transport = () => assert.fail("must not reach network");
  await assert.rejects(callback("claim", {}, { ...environment, CODEY_RELEASE_ADMIN_URL: "http://example.com" }, transport), /HTTPS/);
  await assert.rejects(callback("claim", {}, { ...environment, RELEASE_ADMIN_CALLBACK_SECRET: "short" }, transport), /至少/);
});

test("callback rejects authorization and stale execution without automatic replay", async () => {
  let calls = 0;
  await assert.rejects(callback("claim", {}, environment, async () => {
    calls += 1;
    return new Response("stale attempt", { status: 409 });
  }), /409.*stale attempt/);
  assert.equal(calls, 1);
});

test("claim data must match repository, version, source and comparison range", () => {
  assert.equal(validateBuild(build, environment), build);
  for (const change of [{ repository: "attacker/repo" }, { source_sha: "c".repeat(40) }, { base_sha: "c".repeat(40) }, { version: "2.0.0" }, { tag: "v9.0.0" }, { action: "delete" }, { attempt: 3 }]) {
    assert.throws(() => validateBuild({ ...build, ...change }, environment), /不一致/);
  }
  assert.throws(() => validateBuild({ ...build, artifact_run_id: 123 }, environment), /产物来源/);
  assert.equal(validateBuild({ ...build, artifact_run_id: 123 }, { ...environment, RELEASE_ARTIFACT_RUN_ID: '123' }).artifact_run_id, 123);
  for (const version of ["01.2.3", "1.2.3-01", "1.2.3-rc..1", "1.2.3+build", "9007199254740992.2.3"]) assert.throws(() => validateBuild({ ...build, version, tag: `v${version}` }, { ...environment, RELEASE_VERSION: version }), /SemVer/);
});

test("release ownership rejects collisions and unexpected IDs", () => {
  const release = { id: 10, tag_name: "v1.2.3", body: "notes\n<!-- codey-build:build_test-123 -->" };
  assert.equal(validateRelease(release, build), release);
  assert.throws(() => validateRelease({ ...release, body: "other release" }, build), /占用/);
  assert.throws(() => validateRelease(release, { ...build, release_id: 11 }), /占用/);
});

test("retry cannot overwrite changed or missing installation assets", () => {
  const assets = [{ file_name: "installer.exe", sha256: "a".repeat(64), size: 42 }];
  assertSameAssets(assets, structuredClone(assets));
  assert.throws(() => assertSameAssets(assets, [{ ...assets[0], sha256: "b".repeat(64) }]), /产物/);
  assert.throws(() => assertSameAssets(assets, []), /产物/);
});

test("AI notes require file and literal code evidence from actual differences", () => {
  const diff = "+const retries = 3;\n-const retries = 1;";
  const patches = [{ file: 'retry.js', diff }];
  const valid = { notes: "- 调整失败重试次数", evidence: [{ note: "调整失败重试次数", file: "retry.js", excerpt: "+const retries = 3;" }] };
  assert.equal(validateNotes(valid, patches).notes_status, "generated");
  assert.throws(() => validateNotes({ ...valid, evidence: [{ ...valid.evidence[0], file: "missing.js" }] }, patches), /无法核实/);
  assert.throws(() => validateNotes({ ...valid, evidence: [{ ...valid.evidence[0], excerpt: "unrelated evidence" }] }, patches), /无法核实/);
  assert.throws(() => validateNotes({ ...valid, evidence: [{ ...valid.evidence[0], note: '额外结论' }] }, patches), /无法核实/);
  assert.throws(() => validateNotes({ ...valid, notes: valid.notes + '\n- 无证据的性能提升' }, patches), /每条日志/);
  assert.throws(() => validateNotes({ notes: "无证据的性能提升", evidence: [] }, patches), /证据/);
});

test("managed workflows isolate AI permissions and wait for every build gate before failure reporting", async () => {
  const workflow = await readFile(new URL("../.github/workflows/build-desktop.yml", import.meta.url), "utf8");
  const notes = workflow.slice(workflow.indexOf("\n  notes:"), workflow.indexOf("\n  macos:"));
  assert.match(notes, /contents: read\s+copilot-requests: write/);
  assert.doesNotMatch(notes, /RELEASE_ADMIN_CALLBACK_SECRET|CLOUDFLARE_API_TOKEN|contents: write/);
  assert.match(notes, /persist-credentials: false/);
  const managedPublish = workflow.slice(workflow.indexOf("\n  managed-publish:"), workflow.indexOf("\n  report-failure:"));
  for (const gate of ["prepare", "notes", "macos", "macos-check", "windows", "windows-check"]) assert.match(managedPublish, new RegExp(`- ${gate}\\n`));
  const failure = workflow.slice(workflow.indexOf("\n  report-failure:"));
  assert.match(failure, /needs.prepare.outputs.claimed == 'true'/);
  assert.match(failure, /needs.managed-publish.result != 'success'/);
  for (const gate of ["prepare", "notes", "macos", "macos-check", "windows", "windows-check", "managed-publish"]) assert.match(failure, new RegExp(`- ${gate}\\n`));
});

test("managed platform builds inject claimed versions before Rust caches without changing legacy or artifact-reuse paths", async () => {
  const workflow = await readFile(new URL("../.github/workflows/build-desktop.yml", import.meta.url), "utf8");
  for (const [job, nextJob] of [["macos", "macos-check"], ["windows", "windows-check"]]) {
    const steps = workflow.slice(workflow.indexOf(`\n  ${job}:`), workflow.indexOf(`\n  ${nextJob}:`));
    assert.match(steps, /if: inputs.artifact_run_id == ''/);
    assert.match(steps, /ref: \$\{\{ inputs.source_sha \|\| github.ref \}\}/);
    assert.match(steps, /name: Download claimed platform build\s+if: inputs.build_id != ''\s+uses: actions\/download-artifact@v4\s+with:\s+name: release-build-context/);
    assert.match(steps, /name: Inject validated platform release version\s+if: inputs.build_id != ''\s+run: node scripts\/release-automation.mjs prepare-version/);
    assert.ok(steps.indexOf("prepare-version") < steps.indexOf("name: Cache Rust build"));
    assert.doesNotMatch(steps, /release\.mjs|generate-lockfile|git push|git commit/);
  }
  const notes = workflow.slice(workflow.indexOf("\n  notes:"), workflow.indexOf("\n  macos:"));
  assert.doesNotMatch(notes, /prepare-version/);
  const legacy = workflow.slice(workflow.indexOf("\n  publish:"), workflow.indexOf("\n  managed-publish:"));
  assert.match(legacy, /startsWith\(github.ref, 'refs\/tags\/v'\) && inputs.build_id == ''/);
  assert.doesNotMatch(legacy, /prepare-version/);
});

test("delete maintenance uses the same lock and has no R2 credentials", async () => {
  const workflow = await readFile(new URL("../.github/workflows/release-maintenance.yml", import.meta.url), "utf8");
  assert.match(workflow, /group: codey-release-automation\s+cancel-in-progress: false/);
  assert.match(workflow, /run-name: "Codey .*#\$\{\{ inputs.attempt \}\}"/);
  const deletion = workflow.slice(workflow.indexOf("\n  delete:"), workflow.indexOf("\n  report-failure:"));
  assert.doesNotMatch(deletion, /CLOUDFLARE|copilot/);
  assert.match(deletion, /release-automation.mjs delete/);
});

async function sandboxBuild(action, handler) {
  const directory = await mkdtemp(join(tmpdir(), "codey-release-automation-"));
  const oldCwd = process.cwd();
  const oldEnv = { ...process.env };
  const oldFetch = globalThis.fetch;
  try {
    Object.assign(process.env, environment, { RELEASE_ACTION: action, GH_TOKEN: "test-token" });
    process.chdir(directory);
    await writeFile(".release-build.json", JSON.stringify({ ...build, action, release_id: 10 }));
    await handler(directory);
  } finally {
    globalThis.fetch = oldFetch;
    process.chdir(oldCwd);
    for (const key of Object.keys(process.env)) if (!(key in oldEnv)) delete process.env[key];
    Object.assign(process.env, oldEnv);
    await rm(directory, { recursive: true, force: true });
  }
}

test("claim accepts a platform-selected version different from valid consistent source manifests", async () => {
  for (const action of ["build", "notes", "delete"]) await sandboxBuild(action, async () => {
    await writeFile("package.json", '{"version":"1.2.2"}');
    await writeFile("Cargo.toml", '[workspace.package]\nversion = "1.2.2"\n');
    globalThis.fetch = async () => Response.json({ ...build, action });
    await main("claim");
    assert.equal(JSON.parse(await readFile(".release-build.json", "utf8")).version, "1.2.3");
    assert.equal(JSON.parse(await readFile("package.json", "utf8")).version, "1.2.2");
  });
});

test("claim rejects inconsistent or invalid source versions without writing a claimed context", async () => {
  await sandboxBuild("build", async () => {
    await rm(".release-build.json");
    await writeFile("package.json", '{"version":"1.2.2"}');
    await writeFile("Cargo.toml", '[workspace.package]\nversion = "1.2.1"\n');
    globalThis.fetch = async () => Response.json(build);
    await assert.rejects(main("claim"), /必须一致/);
    await assert.rejects(readFile(".release-build.json"), { code: "ENOENT" });
    await writeFile("package.json", '{"version":"01.2.2"}');
    await assert.rejects(main("claim"), /SemVer/);
    await assert.rejects(readFile(".release-build.json"), { code: "ENOENT" });
  });
});

test("prepare-version rejects maintenance and artifact reuse before changing manifests", async () => {
  for (const action of ["notes", "delete"]) await sandboxBuild(action, async () => {
    await assert.rejects(main("prepare-version"), /完整打包任务/);
  });
  await sandboxBuild("build", async () => {
    process.env.RELEASE_ARTIFACT_RUN_ID = "123";
    await writeFile(".release-build.json", JSON.stringify({ ...build, artifact_run_id: 123 }));
    await assert.rejects(main("prepare-version"), /完整打包任务/);
  });
});

test("delete cleans the owned Release before tag and asks backend to clean R2 last", async () => {
  await sandboxBuild("delete", async () => {
    let hasTag = true;
    let hasRelease = true;
    const mutations = [];
    globalThis.fetch = async (url, options = {}) => {
      const path = new URL(url).pathname;
      const method = options.method || "GET";
      if (path.endsWith("/events")) {
        const body = JSON.parse(options.body);
        assert.equal(body.action, "delete");
        assert.equal(body.source_sha, build.source_sha);
        assert.equal(options.headers["x-release-signature"], signature(options.headers["x-release-timestamp"], options.body, environment.RELEASE_ADMIN_CALLBACK_SECRET));
        mutations.push(body.status);
        return new Response('{"ok":true}');
      }
      if (method === "DELETE") {
        mutations.push(path);
        if (path.endsWith("/releases/10")) hasRelease = false;
        else if (path.endsWith("/git/refs/tags/v1.2.3")) hasTag = false;
        else assert.fail(`unexpected deletion ${path}`);
        return new Response(null, { status: 204 });
      }
      if (path.endsWith("/git/ref/tags/v1.2.3")) return hasTag ? Response.json({ object: { type: "tag", sha: "tag-sha" } }) : new Response(null, { status: 404 });
      if (path.endsWith("/git/tags/tag-sha")) return Response.json({ object: { type: "commit", sha: build.source_sha }, message: `<!-- codey-build:${build.id} -->` });
      if (path.endsWith("/releases/tags/v1.2.3")) return hasRelease ? Response.json({ id: 10, tag_name: build.tag, body: `<!-- codey-build:${build.id} -->` }) : new Response(null, { status: 404 });
      if (path.endsWith("/repos/owner/codey/")) return Response.json({ id: 1 });
      assert.fail(`unexpected request ${url}`);
    };
    await main("delete");
    assert.deepEqual(mutations, ["/repos/owner/codey/releases/10", "/repos/owner/codey/git/refs/tags/v1.2.3", "github_deleted", "clean_r2"]);
    mutations.length = 0;
    await main("delete");
    assert.deepEqual(mutations, ["github_deleted", "clean_r2"]);
  });
});

test("delete refuses a tag pointing at a different commit before any mutation", async () => {
  await sandboxBuild("delete", async () => {
    globalThis.fetch = async (url, options = {}) => {
      assert.notEqual(options.method, "DELETE");
      const path = new URL(url).pathname;
      if (path.endsWith("/git/ref/tags/v1.2.3")) return Response.json({ object: { type: "tag", sha: "tag-sha" } });
      if (path.endsWith("/git/tags/tag-sha")) return Response.json({ object: { type: "commit", sha: "c".repeat(40) }, message: `<!-- codey-build:${build.id} -->` });
      if (path.endsWith("/repos/owner/codey/")) return Response.json({ id: 1 });
      assert.fail(`unexpected request ${url}`);
    };
    await assert.rejects(main("delete"), /归属或提交不一致/);
  });
});

test("delete leaves remaining resources visible when GitHub deletion fails", async () => {
  await sandboxBuild("delete", async () => {
    const calls = [];
    globalThis.fetch = async (url, options = {}) => {
      calls.push([url, options.method]);
      const path = new URL(url).pathname;
      assert.ok(!path.endsWith("/events"));
      if (options.method === "DELETE") return new Response("permission denied", { status: 403 });
      if (path.endsWith("/git/ref/tags/v1.2.3")) return Response.json({ object: { type: "tag", sha: "tag-sha" } });
      if (path.endsWith("/git/tags/tag-sha")) return Response.json({ object: { type: "commit", sha: build.source_sha }, message: `<!-- codey-build:${build.id} -->` });
      if (path.endsWith("/releases/tags/v1.2.3")) return Response.json({ id: 10, tag_name: build.tag, body: `<!-- codey-build:${build.id} -->` });
      if (path.endsWith("/repos/owner/codey/")) return Response.json({ id: 1 });
      assert.fail(`unexpected request ${url}`);
    };
    await assert.rejects(main("delete"), /403/);
    assert.equal(calls.filter(([, method]) => method === "DELETE").length, 1);
    assert.ok(!calls.some(([url]) => url.includes("/git/refs/")));
  });
});

test("first release without baseline preserves a manual-notes result without using AI", async () => {
  await sandboxBuild("build", async () => {
    process.env.RELEASE_BASE_SHA = "";
    process.env.RELEASE_BASE_TAG = "";
    await writeFile(".release-build.json", JSON.stringify({ ...build, base_sha: "", base_tag: "" }));
    globalThis.fetch = () => assert.fail("no remote call for missing baseline");
    await main("notes");
    const result = JSON.parse(await readFile("release-notes.json", "utf8"));
    assert.equal(result.notes_status, "manual_required");
    assert.match(result.reason, /首次发布/);
    assert.equal(result.notes, "");
  });
});

test("managed publication fails immediately if R2 configuration is incomplete", async () => {
  await sandboxBuild("build", async () => {
    delete process.env.CLOUDFLARE_R2_BUCKET;
    globalThis.fetch = () => assert.fail("missing R2 must fail before remote writes");
    await assert.rejects(main("publish"), /缺少 CLOUDFLARE_R2_BUCKET/);
  });
});
