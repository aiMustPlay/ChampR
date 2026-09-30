import { join } from "node:path";
import { createInterface } from "node:readline";
import { BrowserSession } from "./browser.js";
import { AdapterError, type WebChatMessage } from "./deepseek-adapter.js";
import {
  PROTOCOL_VERSION,
  encodeMessage,
  parseRequest,
  type SidecarEvent,
  type SidecarResponse,
} from "./protocol.js";

function write(message: SidecarResponse | SidecarEvent): void {
  process.stdout.write(encodeMessage(message));
}

const profilePath = process.env.CHAMPR_DEEPSEEK_WEB_PROFILE
  ?? join(process.env.APPDATA ?? process.cwd(), "champr", "deepseek-web-profile");
const browser = new BrowserSession(profilePath, (state, message) => {
  write({ event: "state", state, message });
});

write({ event: "ready", version: PROTOCOL_VERSION, state: "starting" });

const lines = createInterface({ input: process.stdin, terminal: false });
lines.on("line", async (line) => {
  if (!line.trim()) return;

  try {
    const request = parseRequest(line);
    if (request.method === "shutdown") {
      await browser.close();
      write({ id: request.id, ok: true, result: { state: "stopped" } });
      process.exitCode = 0;
      lines.close();
      return;
    }

    if (request.method === "open_login" || request.method === "resume") {
      await browser.openLogin();
      write({ id: request.id, ok: true, result: await browser.status() });
      return;
    }
    if (request.method === "status") {
      write({ id: request.id, ok: true, result: await browser.status() });
      return;
    }
    if (request.method === "reset") {
      await browser.reset();
      write({ id: request.id, ok: true, result: { state: "ready" } });
      return;
    }
    if (request.method === "send") {
      const messages = request.params?.messages;
      if (!Array.isArray(messages)) throw new AdapterError("invalid_request", "send.params.messages must be an array");
      const timeoutMs = Number(request.params?.timeout_ms ?? 90_000);
      const content = await browser.send(messages as WebChatMessage[], timeoutMs);
      write({ id: request.id, ok: true, result: { content } });
      return;
    }
  } catch (error) {
    write({
      id: "",
      ok: false,
      error: {
        code: error instanceof AdapterError ? error.code : "sidecar_error",
        message: error instanceof Error ? error.message : String(error),
      },
    });
  }
});

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, async () => {
    await browser.close();
    process.exit(0);
  });
}