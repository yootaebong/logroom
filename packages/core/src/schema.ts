import { z } from "zod";

/**
 * Event/Stream 스키마 SSOT (docs/02-data-model.md, docs/03-capture.md).
 * TS 타입은 이 zod 스키마에서 z.infer로 파생한다. Rust 구조체는 이 스키마와
 * 계약 테스트로 동기화한다(docs/01-architecture.md "스키마 SSOT" 참고).
 */

/** streams.source — 캡처 소스 식별자 */
export const sourceSchema = z.enum([
  "claude_code",
  "kiro_cli",
  "kiro_ide",
  "gemini_web",
  "manual",
  "slack",
  "github",
  "linear",
  "notion",
]);
export type Source = z.infer<typeof sourceSchema>;

/** streams.kind */
export const streamKindSchema = z.enum(["session", "task", "thread", "agent"]);
export type StreamKind = z.infer<typeof streamKindSchema>;

/** streams.status — active(최근 활동) | done */
export const streamStatusSchema = z.enum(["active", "done"]);
export type StreamStatus = z.infer<typeof streamStatusSchema>;

/** events.type */
export const eventTypeSchema = z.enum([
  "prompt",
  "response",
  "tool_use",
  "tool_result",
  "message",
  "file_edit",
  "note",
]);
export type EventType = z.infer<typeof eventTypeSchema>;

/**
 * Stream — 세션/대화/작업 흐름 = 타임라인 레인 1개 (docs/02 DDL `streams` 테이블).
 * `id`/`createdAt`/`status`는 서버(Rust)가 upsert 시 채우므로 인제스트 페이로드에서는 선택.
 */
export const streamSchema = z.object({
  id: z.string().min(1), // "<source>:<native session id>"
  source: sourceSchema,
  kind: streamKindSchema.default("session"),
  title: z.string().nullable().optional(),
  project: z.string().nullable().optional(), // cwd/repo 경로
  gitBranch: z.string().nullable().optional(),
  startedAt: z.number().int().nonnegative().optional(), // epoch ms, 서버가 첫 이벤트로 채울 수 있음
  endedAt: z.number().int().nonnegative().nullable().optional(), // epoch ms
  status: streamStatusSchema.optional(),
  metadata: z.record(z.string(), z.unknown()).default({}),
});
export type Stream = z.infer<typeof streamSchema>;

/**
 * Event — 스트림 내 개별 활동 (docs/02 DDL `events` 테이블).
 * `externalId`는 (source, externalId) UNIQUE로 재수집 idempotent를 보장하는 dedup 키(필수).
 */
export const eventSchema = z.object({
  externalId: z.string().min(1), // 소스 native uuid (dedup 키)
  streamId: z.string().min(1),
  ts: z.number().int().nonnegative(), // epoch ms
  source: sourceSchema,
  type: eventTypeSchema,
  title: z.string().nullable().optional(),
  body: z.string().nullable().optional(),
  model: z.string().nullable().optional(),
  tokensIn: z.number().int().nonnegative().nullable().optional(),
  tokensOut: z.number().int().nonnegative().nullable().optional(),
  url: z.string().nullable().optional(),
  parentId: z.string().nullable().optional(), // 스레드 트리(부모 이벤트 id, external_id 기준)
  metadata: z.record(z.string(), z.unknown()).default({}),
});
export type Event = z.infer<typeof eventSchema>;

/**
 * POST /v1/ingest 바디 (docs/03-capture.md "인제스트 API 계약").
 * `stream`은 선택 — 있으면 upsert. `events`는 1건 이상의 배치.
 */
export const ingestRequestSchema = z.object({
  stream: streamSchema.optional(),
  events: z.array(eventSchema).min(1),
});
export type IngestRequest = z.infer<typeof ingestRequestSchema>;

/** GET /v1/health 응답 (docs/03-capture.md) */
export const healthResponseSchema = z.object({
  ok: z.boolean(),
  version: z.string(),
});
export type HealthResponse = z.infer<typeof healthResponseSchema>;
