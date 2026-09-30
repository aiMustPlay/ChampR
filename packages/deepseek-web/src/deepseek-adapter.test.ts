import assert from "node:assert/strict";
import test from "node:test";
import { classifyVisibleText, formatMessages } from "./deepseek-adapter.js";

test("formats role-labelled conversation text", () => {
  assert.equal(
    formatMessages([
      { role: "system", content: "只用中文" },
      { role: "user", content: "  怎么打团？ " },
    ]),
    "系统要求: 只用中文\n\n用户: 怎么打团？",
  );
});

test("classifies login and verification states", () => {
  assert.equal(classifyVisibleText("密码登录"), "login_required");
  assert.equal(classifyVisibleText("Abnormal usage environment"), "paused_needs_user");
  assert.equal(classifyVisibleText("正常聊天内容"), undefined);
});