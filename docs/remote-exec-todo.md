# 远程命令执行（remote-exec）— 待办 TODO

> 状态:待排期  ·  记录日期:2026-08-04  ·  范围:`toolkit-worker` · `toolkit-server` · `zero-desktop`
>
> 关联:[remote-exec-design.md](remote-exec-design.md)(设计,第一期已落地) ·
> [identity.rs](../crates/toolkit-worker/src/identity.rs) ·
> [exec/routes.rs](../crates/toolkit-server/src/exec/routes.rs) ·
> [ExecNodesPage.tsx](../crates/zero-desktop/ui/src/modules/exec/ExecNodesPage.tsx)

本文件记录第一期落地后暴露的问题与待补能力,逐条「现象 / 判断 / 待办」。完成一项勾掉 `[ ]`
并补「落地说明」。

---

## TODO-1 桌面端看不到申请、也没有审批动作

- [ ] **现象**(2026-08-04):外部机器上的 worker 已经在反复提交申请,但 zero-desktop 上既没有
  待审批记录,也没有可以批准的地方。

- **判断**:**当前运行的两端都是旧版本**,不是逻辑缺陷:
  - G10 上跑的 `toolkit-server` 是 2026-08-03 上午部署的那版,**早于**申请/审批端点
    (`/api/internal/exec/access/*`、`/api/web/exec/requests`)的实现,所以这些路由在现网根本不存在;
  - 本机运行的 zero-desktop 也是旧构建,「远程节点」页(`exec` 模块 + `ExecNodesPage`)是之后才加的。
- **待办**:
  1. 重新交叉编译部署 `toolkit-server` 到 G10(env 需带 `TOOLKIT_EXEC_TOKEN`,否则
     `/api/web/exec/*` 根本不挂载;同时**别漏** `TTS_BASE_URL` / `CLEAN_BASE_URL` /
     `GOP_BASE_URL`——`install` 会整份重写 unit,清单里没有的变量会被抹掉);
  2. 重新构建并启动 zero-desktop,在设置页填「远程执行 Token」(独立于 G10 Bearer Token);
  3. 外部机器换上新版 `toolkit-worker`(id 派生规则与配置位置都变了,旧 worker 的 `w-228` /
     `w-1001` 之类 id 不再适用),跑 `toolkit-worker run` 走一遍完整的申请 → 批准闭环。
- **验收**:worker 日志出现「等待批准中」→ 面板出现待审批卡片 → 点批准 20 小时 → worker 自动
  转入主循环 → `/run` 能跑出结果。

---

## TODO-2 worker 轮询申请结果时,非 200 响应被当成 JSON 解析

- [ ] **现象**(2026-08-04,外网入口):

  ```text
  [WARN][toolkit_worker::identity:226] 查询申请结果失败: 解析申请结果失败: error decoding
  response body for url (https://spark.for-memory.site:38788/api/internal/exec/access/poll
  ?worker_id=w-2dff949bcdd54ae0): EOF while parsing a value at line 1 column 0
  ```

- **判断**:**这是 worker 侧的真实健壮性缺陷**,与 TODO-1 的版本问题叠加显形。
  [`identity::poll_request`](../crates/toolkit-worker/src/identity.rs) 拿到响应后**不看状态码**
  直接 `resp.json::<ExecAccessPollResp>()`;现网 server 没有该路由 → 返回 **404 + 空 body** →
  解析空串必然 `EOF while parsing a value at line 1 column 0`。报错信息把「服务端没有这个接口」
  伪装成了「响应解析失败」,排查时会往错误方向找。
  - 对照:`submit_request` 是按状态码分支处理的(200/429/其他),`poll_request` 漏了这一步。
- **待办**:
  1. `poll_request` 先判状态码:404 → 明确提示「controller 不支持临时权限申请,请升级
     toolkit-server」;5xx/其他非 200 → 带状态码的可读错误;仅 200 才解析 JSON;
  2. 同类问题扫一遍 exec 面其余请求(`register` 已 `error_for_status`,`next` 已按状态码分支,
     `result` 待确认);
  3. 补单测:非 200 响应不应产生「解析失败」类错误信息。
- **附带考虑**:轮询失败连续 N 次(尤其是 404)时,把日志降频或改为每分钟提示一次,避免每 10s
  刷屏——当前是每次都 `warn!`。

---

## 备忘:第二期范围(设计文档已写,尚未开工)

异步 `/submit` + `/result`、有界队列与并发、`request_id` 幂等、远程 `cancel`、worker 本地控制面
(`exec-watch` / `exec-stop` / `exec-pause` / `exec-resume`)、后台服务模式、重启与失联收敛。
详见 [remote-exec-design.md](remote-exec-design.md) 第二期章节。

**当前必须记住的能力边界**:凭据到期、`revoke`、面板拒绝——这三者都只能阻止 worker**领取新命令**,
**杀不掉已经在执行的命令**。真正的远程可靠中止在第二期。
