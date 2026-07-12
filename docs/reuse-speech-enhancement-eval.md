# RE-USE 通用语音增强接入评估（中文 ASR 前处理 + 英语跟读）

> 状态：**评估稿 / 实测已完成 v1**（2026-07-07）· 范围：NVIDIA Real-time RE-USE 在 GB10 上的
> 落地实测 + 两个消费场景（中文 ASR、英语跟读 GOP）的接入定位与风险评估。**仅评估，未改代码。**
>
> 关联：CLAUDE.md「语音底座 / GB10 服务清单」（现有 `:8097` audio-cleanup、`:9101` FunASR、`:8098`
> pronunciation-assess）· [english-shadow-gop-design.md](english-shadow-gop-design.md)（GOP 判分）·
> [english-shadow-realtime-design.md](english-shadow-realtime-design.md)（流式跟读）·
> [llm-and-voice-enhancement-plan.md](llm-and-voice-enhancement-plan.md)（ASR 中文优化）·
> streaming-speech `docs/audio-cleanup-api.md` / `server/audio-cleanup/`（清洗服务部署源）。

---

## 0. 一句话结论

**RE-USE 在真实加性噪声/混响下降噪效果显著（SI-SDR +13~15 dB），可作为现有 `:8097` audio-cleanup
的候选增强引擎；但它是"生成式"增强会引入伪影，两个场景的接入策略截然不同——**

| 场景 | 定位 | 判据 | 风险 | 结论 |
|---|---|---|---|---|
| **中文 ASR 前处理** | 脏音频进 FunASR 前先清洗，提升识别率 | 端到端 **CER 下降** | 轻噪/编解码失真下反而可能伤 ASR | **有条件推荐**：按噪声档位门控，A/B 实测 CER 再定 |
| **英语跟读 GOP** | 学员录音进发音评测前先清洗 | 分数**准确性**（不能虚高/虚低） | 增强会改音素声学 → **可能篡改发音分**（把读错的音"脑补"成标准音） | **默认不接 / 高度谨慎**：仅在强噪场景开，且必须证明不污染分数 |
| **商用发布** | —— | —— | **License = NVIDIA NSCLv1 非商用** | **硬阻塞**：仅限研究/内部评估，不可随产品发布 |

---

## 1. 模型是什么（实测事实）

- **模型**：`nvidia/Real-time_RE-USE`（已下到 0.68 `~/la3b/reuse`）。通用语音增强 = 一个模型统一处理
  **噪声 / 混响 / 削波(clipping) / 编码伪影 / 带宽受限**，多语种、多采样率（8k~48k，SFI-STFT 单模型跨采样率）。
- **架构**：卷积编码器 + 卷积解码器 + **Mamba** 做时频建模；~3.7M 参数（另有标准离线版
  `nvidia/RE-USE` ~9.6M / 30 层双向 Mamba，效果更强但只离线）。
- **"一模型多延迟"**：两个旋钮组合出 30 档延迟——
  - `look_ahead_frames` (0~2)：算法延迟（前瞻帧）
  - `Exit_layer` (3~12)：计算延迟（网络早退，用深度换算力）
- **输入/输出**：单声道 wav → 增强后同采样率 flac；论文 *One Model, Many Latencies* (arXiv 2606.25621)。

### GB10 实测速度（纯推理 RTF）

| 档位 | 质量（SI-SDR Δ） | 速度 |
|---|---|---|
| FAST `Exit=3, ahead=0` | +12.9 dB | **37× 实时**（RTF 0.027） |
| QUALITY `Exit=12, ahead=2` | +14.7 dB | **12× 实时**（RTF 0.082） |

`Exit_layer` 旋钮清楚地用算力换质量（37×↔12×），可按延迟预算选档。离线批处理绰绰有余；流式跟读用
低档也能实时，但要叠加在现有 GOP 链路上评估总延迟。

---

## 2. 增强质量实测（SI-SDR / STOI / PESQ）

**方法**：干净语音(24kHz TTS) → 合成重加性噪声（白噪+120Hz 电流声+粉噪，**SNR −5 dB**）→ RE-USE 增强
→ 对齐 clean/noisy/enhanced 三条 16k 信号算客观指标（语音增强标准评测法，与语言无关）。4 条语音样本平均：

| 配置 | SI-SDR | STOI（可懂度） | PESQ（感知质量） |
|---|---|---|---|
| QUALITY 12/2 | **−4.7 → +10.0 dB（+14.7）** | 0.68 → **0.84** | 1.03 → **1.70** |
| FAST 3/0 | −4.7 → +8.2 dB（+12.9） | 0.68 → 0.80 | 1.03 → 1.48 |

三个指标（越高越好）全部大幅提升 → 在**真实加性噪声**下模型正确、有效。

### ⚠️ 诚实的负面发现（决定接入策略）

1. **轻度劣化 + 编解码失真下，增强反而伤 ASR。** 初测用"电话带通 + μ-law 编解码 + 轻噪"劣化，
   FunASR Paraformer 本就鲁棒（几乎不错），增强后 CER 反而升高（把 "ask you" 脑补成 "axy"）。
   **编解码失真不是降噪器的主场**，生成式增强会"补"出伪影。RE-USE 主攻加性噪声/混响。
2. **音乐会被正确抑制成近静音**（demucs 测试轨 SI-SDR → −44 dB、STOI≈0）。这是语音增强器的正确行为，
   但说明它**只对语音有意义**；去 BGM/音乐要靠现有 demucs 路（见 §3）。
3. **别用 ASR-CER 当 SE 指标**：模型/语言一错位指标就失真（Paraformer 是中文模型，样本是英文 TTS，
   CER 全乱）。SE 评测用 SI-SDR/STOI/PESQ 对齐信号；ASR/GOP 的价值要在**各自的端到端指标**上单独证。

---

## 3. 在本仓的定位：落进现有 `:8097` audio-cleanup 槽位

**不是新链路。** 本仓已有整套音频清洗管线，RE-USE 只是一个**可替换/可叠加的增强引擎**：

```
消费方                     toolkit-server 代理              GB10 清洗服务
─────────                  ────────────────────            ─────────────────
douyin(clean_audio=true) ─▶ /api/web/audio/clean ──────────▶ :8097 /clean（现: 人声分离/降噪/
zero-desktop            ─▶ (audio-clean-client, CLEAN_BASE_URL)   删停顿/响度归一）
```

- **现状**：`:8097 /clean` 由 streaming-speech 仓维护（`server/audio-cleanup/`），当前偏"**去 BGM/人声分离**"
  （demucs 系）+ 删停顿 + 响度归一。客户端 `crates/audio-clean-client`、代理 `/api/web/audio/clean`、
  环境变量 `CLEAN_BASE_URL` 全都现成。
- **RE-USE 与现有清洗互补，不是替代**：
  - demucs 路：去音乐/BGM、人声分离（RE-USE 对音乐无效，见 §2.2）
  - RE-USE 路：去**环境噪声/混响/削波/低码率**、还原语音清晰度（demucs 不做这些）
  - 理想形态：清洗服务内部 **demucs 去 BGM → RE-USE 去噪去混响 → 响度归一** 串成一条可选管线，
    由请求参数选择开哪几段。
- **接入零改本仓代码**：把 RE-USE 做成 `:8097` 服务内的一个 `mode`（或新起 `:80xx` 兄弟端口 + 新
  `*_BASE_URL`），部署/契约归 **streaming-speech 仓**（照 audio-cleanup/FunASR 的模式）。本仓侧最多
  加一个透传参数。**本文档只评估，接入实现属 streaming-speech 仓。**

---

## 4. 场景一：中文 ASR 前处理

**目标**：脏中文录音（车管/现场/电话/低码率）在进 FunASR（`:9101 /transcribe`）前先过 RE-USE 去噪，
提升识别率。

**接入点**：走现有 `:8097` 清洗（§3）。douyin 的 `process` 任务已有 `clean_audio=true`（去 BGM 提识别率），
可扩一个"去噪档"复用同一开关族；zero-desktop 走 `/api/web/audio/clean` 透传。

**判据**：端到端 **CER 下降**（不是 SI-SDR）。必须拿**真实脏中文录音**做 A/B：`raw → FunASR` vs
`RE-USE → FunASR`，比 CER。

**门控策略（关键）**：§2.1 已证轻噪/编解码下增强会**伤** ASR。所以：
- **按噪声档位触发**：估计输入 SNR（或用清洗服务返回的噪声度量），只在**明显强噪/混响**时开增强；
  干净/轻噪直接跳过。
- 档位用 QUALITY（离线不缺算力，12× 实时够快）。
- 编解码失真为主的样本（电话 8k μ-law）单独评估，可能要关增强或换标准 RE-USE(9.6M)。

**预期**：强噪场景 CER 明显下降（与 +14.7 dB SI-SDR / +0.16 STOI 一致）；轻噪场景持平或略降，故必须门控。

---

## 5. 场景二：英语跟读 GOP 前处理（高度谨慎）

**目标**：学员录音进发音评测（`:8098 /assess`，wav2vec2 GOP，见
[english-shadow-gop-design.md](english-shadow-gop-design.md)）前先清洗，减少环境噪声对评分的干扰。

### ⚠️ 核心风险：增强会篡改发音分

GOP 评的是"**这个音发得像不像标准音**"——直接读音素段的声学后验。而 RE-USE 是**生成式**增强：
它会朝"干净、标准语音"的先验去重建信号。这带来一个**评测特有的致命偏差**：

- **虚高**：学员把 `think` 的 /θ/ 读成 /s/，增强模型可能朝更"标准"的语音流形重建，**把发音缺陷抹平**
  → GOP 给了不该给的高分（§2.1 里 "ask you"→"axy" 已是同类脑补的实证）。
- **虚低**：增强在音素边界引入伪影 → 后验分下降，把发对的音判成发错。

无论哪种，**增强都在评测对象（发音）上动了手脚**，这与 ASR（只关心内容可懂度）根本不同。ASR 里
增强是净收益/中性；GOP 里增强可能**直接污染要测的量**。

### 建议

- **默认不接。** 安静环境（跟读的标准前提就是"戴耳机/播完再录"，见 realtime 设计 §2 决策 7）本就
  没多少噪声，增强弊大于利。
- **仅在确有强噪的场景做实验性开启**，且必须先证明：
  1. **不改变发音分的相对排序**（正确发音 vs 错误发音的分差在增强前后保持）——用 speechocean762 或
     自采"对/错发音配对"做 A/B，看增强是否压缩了对错分差。
  2. **权威分只认批量 finalizer**（realtime 设计已有此机制）——增强至多用于流式临时分的体验优化，
     落库/权威分走原始录音，避免增强污染统计。
- **替代思路**（更稳）：与其增强波形再评分，不如让 GOP 声学模型**本身对噪声更鲁棒**（训练加噪增广），
  或只用 RE-USE 做**前端 VAD/信噪比闸门**（太脏就提示用户重录），而不动送评的波形。

---

## 6. License / 合规（硬阻塞）

- **RE-USE 与标准 RE-USE 均为 NVIDIA One-Way Noncommercial License (NSCLv1)——仅研究/开发，禁止商用。**
- 对本仓（面向生产的工具链）：**可用于内部评估、离线洗数据、方法验证、写结论**；**不可**随产品/服务对外发布。
- 若两场景任一要进生产，需换**可商用**的语音增强模型（如自训、或商用许可的 SE 模型），把 RE-USE 的结论
  当作"效果上限参照"。这与 [LocateAnything-3B](../CLAUDE.md) 那条非商用模型的处理口径一致：**只做评估基线，
  不进生产解题**。

---

## 7. 建议与下一步（决策判据）

| # | 动作 | 产出 | 判据 |
|---|---|---|---|
| 1 | **中文 ASR A/B**：真实脏中文录音 `raw vs RE-USE→FunASR` 比 CER，按 SNR 分档 | CER 对比表 | 强噪档 CER 显著下降 → 值得在 `:8097` 加去噪 mode |
| 2 | **GOP 分数偏差实验**：对/错发音配对，测增强前后对错分差是否被压缩 | 分差保持率 | 分差不塌 → 才谈在强噪下接；否则否决 |
| 3 | **清洗服务串联评估**（streaming-speech 仓）：demucs 去 BGM → RE-USE 去噪 → 响度归一 是否互补增益 | 管线设计 | 端到端指标优于任一单段 |
| 4 | **合规选型**：确认生产用的可商用 SE 替代；RE-USE 仅留基线 | 选型结论 | —— |

**默认结论**：ASR 前处理**值得按门控接入**（先做动作 1 定量）；英语跟读**默认不接**，除非动作 2 证明分数不被污染。
两者都因 License **不能进生产**，当前仅作内部评估基线。

---

## 附录 A：在 0.68 上复现（GB10 / aarch64）

模型在 `~/la3b/reuse`（`la3b` 容器内 `/work/reuse`，root 所有）。系统 Python 无 torch，复用 gemma 镜像跑：

```bash
# 1. 一次性容器：复用 gemma 镜像(torch cu13 + nvcc)，--network host 走 7890 代理装包
docker run -d --name reuse-run --gpus all --network host \
  -e http_proxy=http://127.0.0.1:7890 -e https_proxy=http://127.0.0.1:7890 \
  -v ~/la3b/reuse:/work -w /work gemma4-26b-a4b sleep infinity

# 2. 装依赖（causal-conv1d / mamba_ssm ARM 无预编译轮子，从源码编译各几分钟）
docker exec reuse-run bash -lc '
  export MAX_JOBS=8 TORCH_CUDA_ARCH_LIST=12.1 CAUSAL_CONV1D_FORCE_BUILD=TRUE MAMBA_FORCE_BUILD=TRUE
  pip install librosa soundfile scipy
  pip install --no-build-isolation causal-conv1d mamba_ssm'

# 3. 两个必修补丁（已存 ~/la3b/reuse）：
#    - 本地 utils/ 被 site-packages 同名命名空间包遮蔽 → 加 utils/__init__.py
#    - torchaudio 2.11 load/save 需 torchcodec → 改用 soundfile（补丁版 offline_inference_sf.py）

# 4. 跑增强
docker exec reuse-run bash -lc 'cd /work && CUDA_VISIBLE_DEVICES=0 python3 offline_inference_sf.py \
  --input_folder ./noisy_audio --output_folder ./enh \
  --config recipes/USEMamba_12x1_*.yaml --Exit_layer 12 --look_ahead_frames 2'
```

清理：`docker rm -f reuse-run`。中文 ASR 转写用 `server-asr-1` 容器里的 FunASR Paraformer（`/models/...`），
或本仓 `:9101 /transcribe`。SE 指标脚本 `se_metrics.py`（SI-SDR/STOI/PESQ）在 `~/la3b/reuse`。
