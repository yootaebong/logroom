import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { ingestRequestSchema } from "../src/schema";

/**
 * 인제스트 계약 테스트 (zod 측).
 * 같은 fixture 를 Rust(src-tauri/src/model.rs)도 serde 로 파싱한다 → 양방향 계약 검증.
 * docs/01-architecture.md "스키마 SSOT" 참고.
 */
const here = dirname(fileURLToPath(import.meta.url));
const raw: unknown = JSON.parse(readFileSync(join(here, "fixtures/ingest-sample.json"), "utf8"));

describe("ingest 계약", () => {
  it("공유 fixture 가 ingestRequestSchema 를 통과한다", () => {
    const parsed = ingestRequestSchema.parse(raw);
    expect(parsed.events).toHaveLength(2);
    expect(parsed.stream?.source).toBe("claude_code");
    expect(parsed.events[0].type).toBe("prompt");
    expect(parsed.events[1].tokensIn).toBe(1200);
    expect(parsed.events[1].model).toBe("claude-sonnet-4-5");
  });
});
