import assert from "node:assert/strict";
import test from "node:test";
import { parseRequest } from "./protocol.js";

test("parses supported requests", () => {
  assert.deepEqual(parseRequest('{"id":"1","method":"status"}'), { id: "1", method: "status" });
});

test("rejects unsupported methods", () => {
  assert.throws(() => parseRequest('{"id":"1","method":"unknown"}'), /unsupported request method/);
});