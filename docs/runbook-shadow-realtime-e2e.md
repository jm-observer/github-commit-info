# 跟读实时发音评测(流式 GOP)端到端验收 runbook(Phase 4)

> 配套设计:[english-shadow-realtime-design.md](english-shadow-realtime-design.md)。
> 契约:streaming-speech `docs/pronunciation-assess-api.md`(`/assess/stream`)。
> 目标:把方案④的整条链路在真机跑通并验收——**桌面采集 → toolkit 中继 → :8098 流式
> (在线 Viterbi)→ partial 逐词点亮 + final 权威分落库**,并确认未启用时零回归。

分三层验收,**从内到外**(任一层不过先修该层,别往外查)。命令均已在开发中实测可用。

---

## 0. 前置:服务就绪 + 部署

```bash
# GB10(192.168.0.68)上:发音评测 + toolkit-server 都在跑
ssh fengqi@192.168.0.68 'docker ps --filter name=pronunciation-assess --format "{{.Names}} {{.Status}}"; \
  curl -s http://127.0.0.1:8098/health; echo; \
  curl -s http://127.0.0.1:8788/api/web/health; echo'
# 期望:pronunciation-assess Up;:8098 {"model_loaded":...,"gpu":true};:8788 {"status":"ok"}
```

**redeploy(本次 Phase 2b 中继 + Phase 3 前端要真机验,必须先重部署 toolkit-server)**:

```powershell
# D:\git\toolkit:交叉编译 + 部署含 shadow/stream.rs 的新版
pwsh ./deploy-g10.ps1 -Service toolkit-server
# 确认 GOP_BASE_URL 已注入(systemd drop-in 或部署面板环境变量),否则流式端点 503
ssh fengqi@192.168.0.68 'pid=$(pgrep -f toolkit-server|head -1); tr "\0" "\n" </proc/$pid/environ | grep GOP_BASE_URL'
# 期望:GOP_BASE_URL=http://127.0.0.1:8098
```

> 桌面端:`cd crates/zero-desktop/ui && npm run build` 出带 Phase 3 的构建,或 `npm run dev` 起开发壳。
> 设置页「连接地址」指向 GB10 局域网(http/ws :8788)。

---

## 1. Tier 1 — 直连 `:8098` 流式 WS(发音评测引擎本身)

容器内用测试客户端,实时节奏喂 wav,看 partial 渐进 + final。

```bash
ssh fengqi@192.168.0.68 'C=pronunciation-assess-pronunciation-assess-1; \
  # 用 TTS 合一句英文(或上传真人 wav 到容器 /tmp)
  curl -s -X POST http://127.0.0.1:8095/tts -H "Content-Type: application/json" \
    -d "{\"text\":\"The quick brown fox jumps over the lazy dog\",\"voice_id\":\"edge_yunxi\"}" \
    -o /tmp/s.wav && docker cp /tmp/s.wav $C:/tmp/s.wav >/dev/null; \
  docker exec $C python3 /app/test_stream_client.py /tmp/s.wav \
    "The quick brown fox jumps over the lazy dog" --realtime'
```

**期望**:`ready` →(随"说话")`partial 词#0 The …`/`#1 quick …` 渐进打印 → `FINAL 句分=… bad=…` + 逐词分。

**故意错读检测**(关键):合成 `I sink so`,按 `I think so` 评,确认 TH 仍判 bad:

```bash
ssh fengqi@192.168.0.68 'C=pronunciation-assess-pronunciation-assess-1; \
  curl -s -X POST http://127.0.0.1:8095/tts -H "Content-Type: application/json" \
    -d "{\"text\":\"I sink so\",\"voice_id\":\"edge_yunxi\"}" -o /tmp/err.wav && docker cp /tmp/err.wav $C:/tmp/err.wav >/dev/null; \
  docker exec $C python3 /app/test_stream_client.py /tmp/err.wav "I think so" --realtime'
# 期望:final 里 think 的 TH=0.x bad(/θ/ 读成 /s/ 被定位)
```

✅ **Tier 1 通过判据**:ready/partial/final 三类事件齐全;partial 随时间渐进到达(非一次性);
final 句分合理;故意错读的目标音素判 bad。

---

## 2. Tier 2 — toolkit-server WS 中继(`/api/web/shadow/stream`)+ 落库

中继把桌面端 ↔ `:8098` 双向转发,并把 `final` 规范化为 `ScoreResult` + 落 `shadow_attempt`。
在 GB10 上用一段最小 Python 连**中继**(注意路径 `/api/web/shadow/stream` + query 元信息):

```bash
ssh fengqi@192.168.0.68 'C=pronunciation-assess-pronunciation-assess-1; docker exec $C python3 - <<"PY"
import asyncio, json, numpy as np, gop
from aiohttp import ClientSession
async def main():
    wav=gop._load_audio_16k("/tmp/s.wav").numpy(); pcm=(np.clip(wav,-1,1)*32767).astype(np.int16)
    ref="The quick brown fox jumps over the lazy dog"
    url=("ws://127.0.0.1:8788/api/web/shadow/stream"
         "?customer_id=1&kind=sentence&sentence_id=99999")
    async with ClientSession() as s, s.ws_connect(url) as ws:
        await ws.send_json({"type":"hello","ref_text":ref,"granularity":"sentence"})
        async def rd():
            async for m in ws:
                d=json.loads(m.data)
                if d["type"]=="final": print("FINAL score=",d.get("score"),"passed=",d.get("passed"),"bad=",d.get("bad_phone_count")); return
                if d["type"]=="partial": print("partial",d["word_index"],d["ref"],d.get("pron_status"))
                if d["type"]=="error": print("ERR",d.get("message")); return
        r=asyncio.ensure_future(rd())
        ch=int(0.32*16000)
        for i in range(0,len(pcm),ch):
            await ws.send_bytes(pcm[i:i+ch].tobytes()); await asyncio.sleep(0.32)
        await ws.send_json({"type":"end"}); await r
asyncio.get_event_loop().run_until_complete(main())
PY'
```

**期望**:`final` 事件带 **`score`/`passed`**(中继已规范化成批量同形,**非** `sentence_score`)。

**落库校验**:

```bash
ssh fengqi@192.168.0.68 'sqlite3 ~/.config/toolkit-server/toolkit.db \
  "SELECT id,kind,sentence_id,round(score,3),passed,substr(detail_json,1,40) \
   FROM shadow_attempt WHERE sentence_id=99999 ORDER BY created_at DESC LIMIT 1;"'
# 期望:有一行,score/passed 与 final 一致,detail_json 非空(GOP 音素明细)
```

**未配回退校验**(可选):临时停掉 GOP_BASE_URL → 连中继应得 503/`error`(流式无 v1 回退);
而批量 `/api/web/shadow/score` 仍回退 v1。

✅ **Tier 2 通过判据**:中继 final 为 `ScoreResult` 形(`score`/`passed`/`words[].pron_status`);
`shadow_attempt` 落到权威分行;未配 GOP_BASE_URL 时流式端点 503。

---

## 3. Tier 3 — 桌面端 UI(边读边评)

1. 起桌面(`npm run dev` 或安装包),英语模块进跟读面板,设置指向 GB10 局域网。
2. 勾「**流式评测**」开关(默认关)。开「开启」跟读。
3. 播放参考音频 → 跟读一句。

**期望**:
- 录音中,参考文本逐词**渐进点亮**(tentative 半透明 + 悬浮"评估中 xx%")。
- 整句读完 ~1s 内,逐词变**权威色**(绿/黄/红)+ 句分;错读音素红色 + hint。
- 通过且开「通过即自动跳」→ 自动进下一句。
- 成功/失败计数刷新。

**降级校验**:把设置指向一个**没配 GOP_BASE_URL** 的 toolkit-server(或关掉流式开关)→ 跟读应
**无缝走批量**(录完整段再出分),功能不坏。这验证 `streamScore` 返回 `null` 的回退路径。

✅ **Tier 3 通过判据**:逐词渐进点亮 + 整句权威分落定 + 错读高亮;流式不可用时零回归走批量。

---

## 4. 验收 checklist

- [ ] Tier 1:`:8098 /assess/stream` partial 渐进 + final;故意错读 TH 判 bad。
- [ ] Tier 2:中继 final 为 `ScoreResult` 形;`shadow_attempt` 落权威分 + detail_json;未配 → 503。
- [ ] Tier 3:桌面边读边点亮 + 落定 + 错读 hint;流式不可用零回归走批量。
- [ ] 延迟体感:partial 跟得上嘴(参考 Phase 0.5 ~80–260ms);final 整句后 ~1s。
- [ ] 回声前提:戴耳机;不戴耳机外放时已知会推高分歧(设计 §9),记录现象。

## 5. 收口项(随真机一并做,设计 §10 Phase 1/0.5 欠账)

- **真人音频翻判率**:用真人(清晰 / 中式 / 故意错读)各几条跑 `phase0_streaming_check.py` +
  `phase1_online_align.py`,把"真实翻判率 / live 抖动 / finalize 延迟"钉死(目前全合成样本)。
- **speechocean762 标定**:`/calib/calibration.json` 用真数据重拟 `a/b` + `ok_min/warn_min`,
  让绝对分与通过线符合直觉(默认 sigmoid 占位偏严)。改完重启容器即生效,无需重编译。
- **commit 前 live 抖动 UX**:错读音素 commit 前会抖(设计 §10 Phase 1),确认 tentative 半透明
  渲染体感可接受;必要时调 `streaming.py` 的 `commit_frames`(默认 4)或加滞后阈值。

## 6. 故障排查

| 现象 | 排查 |
|---|---|
| 桌面流式无反应、退回批量 | `english_shadow_stream_url` 返回空(G10 未配)或 WS 连不上;看 Tier 2 是否通 |
| 中继 `error: 流式上游不可达` | `:8098` 没起 / `GOP_BASE_URL` 错;Tier 1 先过 |
| 中继端点 503 | toolkit-server 没注入 `GOP_BASE_URL`(见 §0 redeploy) |
| partial 不渐进、一次性到 | 客户端没按实时节奏喂(`--realtime`);或音频块太大 |
| final 有分但 `shadow_attempt` 无行 | DB 路径 / 权限;看 toolkit-server 日志 `shadow stream final 落库失败` |
| 分数普遍偏低/偏严 | 标定未做(§5);默认占位 sigmoid 偏严,属预期 |
| 不戴耳机分数飘 | 回声污染(设计 §9 硬前提),戴耳机复测 |
