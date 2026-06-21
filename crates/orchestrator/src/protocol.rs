//! 客户端 ↔ 编排层 协议消息(对应 docs/protocol-draft.md,P0)。
//! 文本帧 = 控制/事件 JSON;二进制帧 = 音频(16k PCM s16le)。

use serde::{Deserialize, Serialize};

/// 客户端 → 服务端:连接后第一帧。
/// `protocol`/`sample_rate`/`format`/`language` 是协议契约字段(客户端必发,
/// 见 protocol-draft.md),编排层目前只用 `want_*`;保留以备 language 路由等。
/// `want_secondary` 可选:客户端要求在主模型识别旁,额外用次模型(对比用)
/// 跑一遍相同 PCM,服务端会发回 `Secondary { ref, text }` 事件。
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Hello {
    pub protocol: String,
    pub sample_rate: u32, // 固定 16000
    pub format: String,   // "pcm_s16le"
    pub language: String, // "zh" / "auto" / "en" ...(透传给 ASR 路由)
    pub want_optimize: bool,
    pub want_translate: bool,
    #[serde(default)]
    pub want_secondary: bool,
    /// 合并间隔(毫秒):相邻 VAD 段音频时间间隔小于此值时,服务端把它们的
    /// 原始 ASR 累积进同一条「合并链」整体润色(按 chain_id 回发,客户端整体
    /// 替换),从根上避免历史上文喂回造成的复制重复。与客户端复制 stitch 用
    /// 同一个值。0 或缺省 = 关闭合并,回到逐段独立 + 历史上下文注入的旧行为。
    #[serde(default)]
    pub merge_window_ms: u64,
    /// 可选 W3C traceparent。客户端无法在 WS 升级时塞请求头(浏览器限制)时,
    /// 走 hello 帧字段兜底,串到 zero/桌面端起点的同一棵 trace。
    #[serde(default)]
    pub traceparent: Option<String>,
}

/// 客户端 → 服务端:控制帧(stop / reset)。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientControl {
    Stop,
    Reset,
}

/// 服务端 → 客户端:事件(均 JSON 文本帧)。`type` 标签区分。
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Ready {
        session_id: String,
    },
    Segment {
        id: u64,
        text: String,
        t_start: Option<f32>,
        t_end: Option<f32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        speaker: Option<String>,
        /// 权威墙上时钟（"%Y-%m-%d %H:%M:%S"）= 会话锚点 + 音频偏移 t_start/t_end。
        /// 客户端直接展示，不再用收到时刻自行推算（避免跨会话/合并链区间重叠）。
        #[serde(skip_serializing_if = "Option::is_none")]
        wall_start: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        wall_end: Option<String>,
    },
    Optimized {
        r#ref: u64,
        text: String,
        /// 节 B：状态标记。`None`（字段省略）= 正常成功；`Some("fallback")` = LLM 调用失败 /
        /// 超时，text 回退为 ASR 原文。客户端可据此提示"LLM 不可用，已返回原文"。
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    Translated {
        r#ref: u64,
        text: String,
    },
    /// 次模型对比识别结果。客户端按 `ref` 与主段 `Segment.id` 关联,在同一
    /// 行下方展示;不参与后续优化/翻译流水线。
    Secondary {
        r#ref: u64,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
    },
    Error {
        code: String,
        message: String,
        fatal: bool,
    },
    Done {
        session_id: String,
    },
}

impl ServerEvent {
    pub fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"type":"error","code":"enc","message":"serialize failed","fatal":true}"#.to_string()
        })
    }
}
