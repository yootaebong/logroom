//! Claude Code / Kiro CLI JSONL 파일감시 + 정규화 (docs/03-capture.md 소스1·소스2, ADR-0010).
//! 저장 진실소스는 이 모듈(JSONL 파서)뿐이다. hook은 라이브 신호 전용(영구 저장 안 함).

pub mod config;
pub mod github;
pub mod health;
pub mod hub;
pub mod kiro;
pub mod linear;
pub mod normalize;
pub mod notion;
pub mod policy;
pub mod scrub;
pub mod slack;
pub mod watch;

pub use watch::spawn;
