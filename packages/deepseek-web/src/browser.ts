import { mkdir } from "node:fs/promises";
import { chromium, type BrowserContext, type Page } from "playwright";
import { DeepSeekAdapter, type AdapterState, type WebChatMessage } from "./deepseek-adapter.js";

const CHAT_URL = "https://chat.deepseek.com/";

export class BrowserSession {
  private context?: BrowserContext;
  private page?: Page;
  private adapter?: DeepSeekAdapter;
  private syncedMessages: WebChatMessage[] = [];

  constructor(
    private readonly profilePath: string,
    private readonly emitState: (state: AdapterState, message?: string) => void,
  ) {}

  async start(): Promise<void> {
    if (this.context) return;
    await mkdir(this.profilePath, { recursive: true });
    this.context = await chromium.launchPersistentContext(this.profilePath, {
      channel: "msedge",
      headless: false,
      viewport: null,
      args: ["--start-maximized"],
    });
    this.page = this.context.pages()[0] ?? (await this.context.newPage());
    if (!this.page.url().startsWith("https://chat.deepseek.com")) {
      await this.page.goto(CHAT_URL, { waitUntil: "domcontentloaded" });
    }
    this.adapter = new DeepSeekAdapter(this.page);
    await this.publishStatus();
  }

  async openLogin(): Promise<void> {
    await this.start();
    await this.page!.bringToFront();
    await this.page!.goto(CHAT_URL, { waitUntil: "domcontentloaded" });
    await this.publishStatus();
  }

  async status(): Promise<{ state: AdapterState; message?: string }> {
    await this.start();
    return this.adapter!.status();
  }

  async send(messages: WebChatMessage[], timeoutMs: number): Promise<string> {
    await this.start();
    this.emitState("generating");
    try {
      const hasSyncedPrefix = messages.length >= this.syncedMessages.length
        && this.syncedMessages.every((message, index) =>
          message.role === messages[index]?.role && message.content === messages[index]?.content
        );
      if (!hasSyncedPrefix) {
        await this.adapter!.reset();
        this.syncedMessages = [];
      }
      const pendingMessages = messages.slice(this.syncedMessages.length);
      if (pendingMessages.length === 0) {
        throw new Error("no new messages to send");
      }
      const content = await this.adapter!.send(pendingMessages, timeoutMs);
      this.syncedMessages = [...messages, { role: "assistant", content }];
      this.emitState("ready");
      return content;
    } catch (error) {
      const state = error instanceof Error && "code" in error ? String(error.code) as AdapterState : "incompatible";
      this.emitState(state, error instanceof Error ? error.message : String(error));
      throw error;
    }
  }

  async reset(): Promise<void> {
    await this.start();
    await this.adapter!.reset();
    this.syncedMessages = [];
    await this.publishStatus();
  }

  async close(): Promise<void> {
    await this.context?.close();
    this.context = undefined;
    this.page = undefined;
    this.adapter = undefined;
    this.syncedMessages = [];
  }

  private async publishStatus(): Promise<void> {
    const status = await this.adapter!.status();
    this.emitState(status.state, status.message);
  }
}