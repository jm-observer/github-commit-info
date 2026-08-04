# 远程运维通道（remote-ops）设计

> 把 `toolkit-worker` 从「出口代理执行端」提级为「远程节点代理」：对方装上一个 worker，
> 中心（G10 toolkit-server）即可反向连上其所在局域网的机器，执行命令、取文件、开隧道，
> 用于**交易模拟项目的远程问题排查**。
>
> 关联文档：[distributed-worker-design.md](distributed-worker-design.md)（worker/出口池底座）。

---

## 1. 背景与目标

### 1.1 现状（已有的底座）

`toolkit-worker` 已经解决了「穿透 NAT 连到对方机器」这个最难的部分：

- **pull 模型**：worker 主动出站连 controller（`--controller http://<公网>:8788`），
  `register` → 10s 心跳 → 长轮询 `/api/internal/egress/next` 取活儿 → 执行 → `POST /egress/result` 回传。
  对方**不需要公网 IP、不需要开入站端口、不需要动防火墙**。
- **鉴权**：共享 token（`EGRESS_WORKER_TOKEN` / 请求头 `x-egress-token`）。
- **注册表**：`egress_pool::Registry`（in-memory）管 worker 在线状态、请求路由、session 绑定。
- **观测面**：`/api/web/egress/workers` + zero-desktop 的 egress 页。
- **已有的 TCP 转发能力**：`toolkit-worker proxy` 子命令（CONNECT 隧道 + 明文 HTTP，
  `copy_bidirectional` 双向转发）——隧道功能可以直接复用这段代码。

缺的只是**任务类型**：现在 worker 只会做一件事——代发 HTTP 请