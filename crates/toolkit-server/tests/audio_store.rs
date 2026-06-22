//! audio-store 集成测试：内容寻址上传 + Range 分块下载，全程走真实 HTTP（无外部依赖）。
//! 复用 `common::TestServer` 起本地 axum，验证 POST → GET 全量 / Range / 416 / 幂等。

mod common;
use common::TestServer;

use serde_json::Value;

/// 构造最小合法 PCM WAV（16-bit 单声道 16kHz，含 `samples` 个样本）。
fn minimal_wav(samples: u32) -> Vec<u8> {
    let sample_rate: u32 = 16000;
    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * (bits / 8) as u32;
    let data_len = samples * channels as u32 * (bits / 8) as u32;
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVE");
    v.extend_from_slice(b"fmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&channels.to_le_bytes());
    v.extend_from_slice(&sample_rate.to_le_bytes());
    v.extend_from_slice(&byte_rate.to_le_bytes());
    v.extend_from_slice(&(channels * bits / 8).to_le_bytes());
    v.extend_from_slice(&bits.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    v.extend(std::iter::repeat_n(0u8, data_len as usize));
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audio_store_end_to_end() {
    let s = TestServer::start().await;
    let client = reqwest::Client::new();
    let wav = minimal_wav(16000); // 1.0s

    // 1) 上传 → 拿 id / duration。
    let resp = client
        .post(s.url("/api/web/audio/store?source=manual"))
        .body(wav.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let id = body["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("aud_"), "id={id}");
    assert_eq!(body["bytes"].as_u64().unwrap() as usize, wav.len());
    assert_eq!(body["duration"].as_f64().unwrap(), 1.0);

    // 2) GET 全量（200，Content-Length 正确，Accept-Ranges）。
    let full = client
        .get(s.url(&format!("/api/web/audio/store/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(full.status(), 200);
    assert_eq!(full.headers()["content-type"], "audio/wav");
    assert_eq!(full.headers()["accept-ranges"], "bytes");
    assert_eq!(
        full.headers()["content-length"]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap(),
        wav.len()
    );
    let full_bytes = full.bytes().await.unwrap();
    assert_eq!(full_bytes.len(), wav.len());
    assert_eq!(&full_bytes[0..4], b"RIFF");

    // 3) GET Range: bytes=0-3 → 206，Content-Range 正确，body 长度=4。
    let part = client
        .get(s.url(&format!("/api/web/audio/store/{id}")))
        .header("Range", "bytes=0-3")
        .send()
        .await
        .unwrap();
    assert_eq!(part.status(), 206);
    assert_eq!(
        part.headers()["content-range"],
        format!("bytes 0-3/{}", wav.len())
    );
    assert_eq!(part.headers()["content-length"], "4");
    let part_bytes = part.bytes().await.unwrap();
    assert_eq!(part_bytes.len(), 4);
    assert_eq!(&part_bytes[..], &wav[0..4]);

    // 4) GET 不可满足 range（start >= size）→ 416 + Content-Range: bytes */size。
    let unsat = client
        .get(s.url(&format!("/api/web/audio/store/{id}")))
        .header("Range", format!("bytes={}-", wav.len() + 10))
        .send()
        .await
        .unwrap();
    assert_eq!(unsat.status(), 416);
    assert_eq!(
        unsat.headers()["content-range"],
        format!("bytes */{}", wav.len())
    );

    // 5) 同字节再上传 → 同 id（内容寻址幂等）。
    let again = client
        .post(s.url("/api/web/audio/store?source=forge"))
        .body(wav.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 200);
    let again_body: Value = again.json().await.unwrap();
    assert_eq!(again_body["id"].as_str().unwrap(), id, "同字节得同 id");

    // 6) 未知 id → 404。
    let missing = client
        .get(s.url("/api/web/audio/store/aud_0000000000000000"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    // 7) 非法 source → 400。
    let bad = client
        .post(s.url("/api/web/audio/store?source=evil"))
        .body(wav.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
}
