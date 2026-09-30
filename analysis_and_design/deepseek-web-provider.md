# DeepSeek Web Provider

## Scope

`deepseek_web` is an experimental AI provider for users who explicitly opt in. It opens a visible Microsoft Edge window with a ChampR-only persistent profile. The user completes login and any later verification; ChampR automates normal text submission, response extraction, and TTS playback.

It does not bypass verification, hide automation, copy credentials, or silently fall back to a paid API.

## Architecture

```text
ChampR request coordinator
  -> crates/lcu BrowserSidecar
  -> NDJSON over child stdin/stdout
  -> packages/deepseek-web
  -> Playwright persistent Edge context
  -> chat.deepseek.com
  -> response text
  -> existing Windows TTS
```

The Rust client serializes requests through one process lock. Protocol messages carry a request ID; asynchronous state events are ignored while waiting for the matching response. Standard error is reserved for sidecar diagnostics.

The default development entry point is `packages/deepseek-web/dist/main.js`. Releases can place the compiled entry point at `deepseek-web/main.js` beside the executable or set `CHAMPR_DEEPSEEK_WEB_SIDECAR_PATH`.

## States

- `starting`: Edge is launching.
- `login_required`: the user must log in in the visible Edge window.
- `ready`: the chat input is available.
- `generating`: a response is being generated.
- `paused_needs_user`: verification or an abnormal-environment warning requires user action.
- `incompatible`: known locators no longer match the page.

Paused and incompatible states never trigger automated retries. The user must use the Settings action to reopen and inspect the browser.

## Privacy

The profile defaults to `%APPDATA%\champr\deepseek-web-profile`. It may contain DeepSeek session cookies and must not be committed or shared. NDJSON and diagnostics must not log cookies, credentials, prompts, or complete responses.

ChampR sends current match context and chat history to the DeepSeek website. The first version does not upload screenshots, audio, or files.

## Service Terms

This integration is not official. DeepSeek's user agreement updated 2025-09-05 states in section 3.5(3) that robots, crawlers, and other automated settings may not scrape or copy service content. The feature is therefore explicit opt-in and experimental. Obtain written authorization before distributing it as a supported production feature.

## Maintenance

All page-specific locators and completion detection live in `packages/deepseek-web/src/deepseek-adapter.ts`. Update that adapter and its local fixture tests when the website changes; never commit a real browser profile or authenticated page capture.