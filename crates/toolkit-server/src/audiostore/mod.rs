//! 音频统一仓库（audio-store，Phase A）：内容寻址的音频 blob 仓库。
//!
//! 把音频字节按 `id`（= sm3 短哈希）收拢到 `<workspace>/audio-store/<id>.wav`，
//! english 等消费方只持 id 引用，消除重复落盘。**只存音频字节本身，不含任何产品语义**
//! （句子 / 课程 / 包等都属消费方）。
//!
//! - [`store`]：存储核心（`blob_id` 内容寻址 + `put` 幂等写入 + DB 元信息）。
//! - [`routes`]：`POST /store` 上传 + `GET /store/{id}`（Range 分块下载）。
//!
//! 见 docs/audio-store-design.md。

pub mod routes;
pub mod store;
