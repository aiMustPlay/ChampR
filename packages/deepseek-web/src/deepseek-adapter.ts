import type { Locator, Page } from "playwright";

export type AdapterState =
  | "starting"
  | "login_required"
  | "ready"
  | "generating"
  | "paused_needs_user"
  | "incompatible";

export interface WebChatMessage {
  role: string;
  content: string;
}

export class AdapterError extends Error {
  constructor(
    public readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

const BLOCKED_TEXT = [
  "Abnormal usage environment",
  "异常使用环境",
  "安全验证",
  "完成验证",
  "验证码",
];

const LOGIN_TEXT = ["手机号登录", "密码登录", "微信扫码登录", "Login with password"];

export function formatMessages(messages: WebChatMessage[]): string {
  return messages
    .filter((message) => message.content.trim())
    .map((message) => {
      const label = message.role === "system" ? "系统要求" : message.role === "assistant" ? "助手" : "用户";
      return `${label}: ${message.content.trim()}`;
    })
    .join("\n\n");
}

export function classifyVisibleText(text: string): AdapterState | undefined {
  if (BLOCKED_TEXT.some((marker) => text.includes(marker))) return "paused_needs_user";
  if (LOGIN_TEXT.some((marker) => text.includes(marker))) return "login_required";
  return undefined;
}

function firstVisible(locators: Locator[]): Promise<Locator | undefined> {
  return (async () => {
    for (const locator of locators) {
      const first = locator.first();
      if (await first.isVisible().catch(() => false)) return first;
    }
    return undefined;
  })();
}

async function inputLocator(page: Page): Promise<Locator | undefined> {
  return firstVisible([
    page.getByRole("textbox", { name: /发送消息|输入消息|message/i }),
    page.locator("textarea"),
    page.locator('[contenteditable="true"]'),
  ]);
}

async function assistantTexts(page: Page): Promise<string[]> {
  const candidates = page.locator(
    '[data-role="assistant"], [data-message-author-role="assistant"], [class*="ds-markdown"], .markdown',
  );
  return candidates.evaluateAll((elements) =>
    elements
      .map((element) => (element as HTMLElement).innerText.trim())
      .filter(Boolean),
  );
}

export class DeepSeekAdapter {
  constructor(private readonly page: Page) {}

  async status(): Promise<{ state: AdapterState; message?: string }> {
    const bodyText = await this.page.locator("body").innerText().catch(() => "");
    const classified = classifyVisibleText(bodyText);
    if (classified) return { state: classified };
    if (await inputLocator(this.page)) return { state: "ready" };
    return { state: "incompatible", message: "未找到 DeepSeek 聊天输入框" };
  }

  async send(messages: WebChatMessage[], timeoutMs: number): Promise<string> {
    const beforeStatus = await this.status();
    if (beforeStatus.state !== "ready") {
      throw new AdapterError(beforeStatus.state, beforeStatus.message ?? `DeepSeek Web 状态: ${beforeStatus.state}`);
    }

    const prompt = formatMessages(messages);
    if (!prompt) throw new AdapterError("invalid_request", "消息内容不能为空");

    const input = await inputLocator(this.page);
    if (!input) throw new AdapterError("incompatible", "未找到 DeepSeek 聊天输入框");
    const before = await assistantTexts(this.page);

    await input.fill(prompt).catch(async () => {
      await input.click();
      await this.page.keyboard.press("Control+A");
      await this.page.keyboard.type(prompt);
    });

    const sendButton = await firstVisible([
      this.page.getByRole("button", { name: /发送|send/i }),
      this.page.locator('button[aria-label*="发送"], button[aria-label*="Send"]'),
    ]);
    if (sendButton) await sendButton.click();
    else await input.press("Enter");

    return this.waitForReply(before.length, timeoutMs);
  }

  async reset(): Promise<void> {
    if ((await assistantTexts(this.page)).length === 0) return;
    const newChat = await firstVisible([
      this.page.getByRole("button", { name: /新建对话|新对话|new chat/i }),
      this.page.getByText(/新建对话|新对话|New chat/i),
    ]);
    if (!newChat) throw new AdapterError("incompatible", "未找到新建对话按钮");
    await newChat.click();
  }

  private async waitForReply(previousCount: number, timeoutMs: number): Promise<string> {
    const deadline = Date.now() + timeoutMs;
    let lastText = "";
    let stableSince = 0;

    while (Date.now() < deadline) {
      const state = await this.status();
      if (state.state === "paused_needs_user" || state.state === "login_required") {
        throw new AdapterError(state.state, `DeepSeek Web 状态: ${state.state}`);
      }

      const replies = await assistantTexts(this.page);
      const latest = replies.length > previousCount ? replies.at(-1)?.trim() ?? "" : "";
      if (latest && latest === lastText) {
        if (stableSince && Date.now() - stableSince >= 1200) return latest.slice(0, 20_000);
      } else {
        lastText = latest;
        stableSince = latest ? Date.now() : 0;
      }
      await this.page.waitForTimeout(250);
    }

    throw new AdapterError("timeout", "等待 DeepSeek Web 回复超时");
  }
}