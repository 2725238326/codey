import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("../public/codey-inject.js", import.meta.url), "utf8");
const start = source.indexOf("  const messageDeletionMethods = {");
const end = source.indexOf("  const isLocalMessageDeletionManager", start);
assert.ok(start >= 0 && end > start);
const methods = Function(`${source.slice(start, end)}; return messageDeletionMethods;`)();

function fixture({ canonical = false } = {}) {
  const first = { turnId: "turn-1", status: "completed", items: [] };
  const second = { turnId: "turn-2", status: "completed", items: [] };
  const conversation = {
    id: "session-1",
    title: "保留标题",
    turns: [first, second],
    requests: [],
    resumeState: "resumed",
    threadRuntimeStatus: { type: "idle" },
    turnsPagination: { olderCursor: "cursor-1", hasLoadedOldest: false },
    latestModel: "model-1",
  };
  if (canonical) {
    conversation.turnHistory = {
      kind: "canonical",
      history: {
        generation: 4,
        isComplete: false,
        entitiesByKey: { "turn:turn-1": first, "turn:turn-2": second },
        islands: [{
          id: "tail:4",
          entries: [
            { key: "turn:turn-1", value: "turn:turn-1" },
            { key: "turn:turn-2", value: "turn:turn-2" },
          ],
          olderBoundary: { status: "available", handle: { cursor: "cursor-1" } },
          newerBoundary: { status: "exhausted" },
        }],
      },
    };
  }
  const requests = [];
  const manager = {
    ...methods,
    role: { role: "owner" },
    getConversation(sessionId) {
      assert.equal(sessionId, conversation.id);
      return conversation;
    },
    getStreamRole() { return this.role; },
    async sendRequest(method, params) { requests.push({ method, params }); },
    updateConversationState(sessionId, update) {
      assert.equal(sessionId, conversation.id);
      update(conversation);
    },
    inactiveThreadUnsubscriber: {
      clearConversationStreamOwnership(sessionId) {
        assert.equal(sessionId, conversation.id);
        manager.role = null;
      },
    },
    discardConversationFromCache() { assert.fail("must retain mounted conversation"); },
    resumeConversation() { assert.fail("must not reload the conversation"); },
  };
  return { conversation, manager, requests, first, second };
}

test("preparing deletion releases only the subscription and preserves the rendered history", async () => {
  const { manager, conversation, requests } = fixture({ canonical: true });
  const before = structuredClone(conversation);
  await manager.prepare(conversation.id);
  assert.deepEqual(conversation, before);
  assert.deepEqual(requests, [{ method: "thread/unsubscribe", params: { threadId: conversation.id } }]);
});

test("in-place deletion retains unselected turn objects and conversation metadata", () => {
  const { manager, conversation, second } = fixture();
  const pagination = conversation.turnsPagination;
  manager.finish(conversation.id, ["turn-1"]);
  assert.deepEqual(conversation.turns, [second]);
  assert.equal(conversation.turns[0], second);
  assert.equal(conversation.turnsPagination, pagination);
  assert.equal(conversation.title, "保留标题");
  assert.equal(conversation.latestModel, "model-1");
  assert.equal(conversation.resumeState, "needs_resume");
  assert.equal(manager.role, null);
});

test("canonical deletion invalidates pending pages while retaining pagination boundaries", () => {
  const { manager, conversation, second } = fixture({ canonical: true });
  const history = conversation.turnHistory.history;
  const island = history.islands[0];
  const olderBoundary = island.olderBoundary;
  const retainedEntry = island.entries[1];
  manager.finish(conversation.id, ["turn-1"]);
  assert.equal(history.generation, 5);
  assert.equal(history.islands[0], island);
  assert.equal(island.olderBoundary, olderBoundary);
  assert.equal(island.entries[0], retainedEntry);
  assert.deepEqual(Object.keys(history.entitiesByKey), ["turn:turn-2"]);
  assert.equal(history.entitiesByKey["turn:turn-2"], second);
  assert.deepEqual(conversation.turns, [second]);
});

test("canonical deletion matches unresolved tail selectors without deleting adjacent records", () => {
  const { manager, conversation } = fixture({ canonical: true });
  const history = conversation.turnHistory.history;
  const unresolvedTurn = { turnId: null, items: [] };
  history.entitiesByKey["tail:4:local:0"] = unresolvedTurn;
  conversation.turns.push(unresolvedTurn);
  history.islands[0].entries.push({ key: "tail:4:local:0", value: "tail:4:local:0" });
  manager.finish(conversation.id, ["history-content:tail:4:local:0"]);
  assert.deepEqual(Object.keys(history.entitiesByKey), ["turn:turn-1", "turn:turn-2"]);
  assert.equal(history.islands[0].entries.length, 2);
  assert.equal(conversation.turns.length, 2);
});

test("deleting all loaded turns retains history boundaries and permits later continuation", () => {
  const { manager, conversation } = fixture({ canonical: true });
  const island = conversation.turnHistory.history.islands[0];
  manager.finish(conversation.id, ["turn-1", "turn-2"]);
  assert.deepEqual(conversation.turns, []);
  assert.deepEqual(conversation.turnHistory.history.entitiesByKey, {});
  assert.deepEqual(island.entries, []);
  assert.equal(conversation.turnHistory.history.islands[0], island);
  assert.equal(conversation.resumeState, "needs_resume");
});

test("failed persistence releases ownership without removing or rebuilding history", () => {
  const { manager, conversation } = fixture({ canonical: true });
  const turns = conversation.turns;
  const before = structuredClone(conversation.turnHistory);
  manager.finish(conversation.id, []);
  assert.equal(conversation.turns, turns);
  assert.deepEqual(conversation.turnHistory, before);
  assert.equal(manager.role, null);
});

test("preparing deletion rejects active tasks followers and unsupported history before unsubscribing", async () => {
  for (const configure of [
    ({ conversation }) => { conversation.threadRuntimeStatus = { type: "active" }; },
    ({ conversation }) => { conversation.turns[0].status = "inProgress"; },
    ({ conversation }) => { conversation.requests.push({ id: "pending" }); },
    ({ manager }) => { manager.role = { role: "follower" }; },
    ({ conversation }) => { conversation.turnHistory = { kind: "unknown" }; },
  ]) {
    const runtime = fixture();
    configure(runtime);
    const before = structuredClone(runtime.conversation);
    await assert.rejects(runtime.manager.prepare(runtime.conversation.id));
    assert.deepEqual(runtime.requests, []);
    assert.deepEqual(runtime.conversation, before);
  }
});
