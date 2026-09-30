export const PROTOCOL_VERSION = 1;

export type RequestMethod =
  | "status"
  | "open_login"
  | "send"
  | "reset"
  | "resume"
  | "shutdown";

export interface SidecarRequest {
  id: string;
  method: RequestMethod;
  params?: Record<string, unknown>;
}

export interface SidecarResponse {
  id: string;
  ok: boolean;
  result?: unknown;
  error?: {
    code: string;
    message: string;
  };
}

export interface SidecarEvent {
  event: string;
  version?: number;
  state?: string;
  message?: string;
}

export function encodeMessage(message: SidecarResponse | SidecarEvent): string {
  return `${JSON.stringify(message)}\n`;
}

export function parseRequest(line: string): SidecarRequest {
  const value: unknown = JSON.parse(line);
  if (!value || typeof value !== "object") {
    throw new Error("request must be an object");
  }

  const request = value as Partial<SidecarRequest>;
  if (typeof request.id !== "string" || !request.id) {
    throw new Error("request id must be a non-empty string");
  }
  if (typeof request.method !== "string") {
    throw new Error("request method must be a string");
  }

  const allowed: RequestMethod[] = [
    "status",
    "open_login",
    "send",
    "reset",
    "resume",
    "shutdown",
  ];
  if (!allowed.includes(request.method as RequestMethod)) {
    throw new Error(`unsupported request method: ${request.method}`);
  }

  return request as SidecarRequest;
}