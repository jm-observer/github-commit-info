# 可用能力：远程在一台 Windows 机器上执行 PowerShell（toolkit remote-exec）

有一台外部 Windows 机器接在 toolkit-server 上，你可以下发 PowerShell 脚本并拿到
stdout / stderr / 退出码。用于远程排查，不是通用 RMM。

## 连接信息

- Controller: `https://spark.for-memory.site:38788`
- 鉴权头: `Authorization: Bearer d5521b68d3314b31ba3140f04bff1d1f`
  （这是 remote-exec 专用 token，与其它 toolkit API 的 token 不是一回事）
- 目标机 worker_id: `w-228`（Windows 11 专业版，PowerShell 5.1，主机名 FENG）
- 你注入的 operator 身份固定是 `fengqi`（由 token 决定，请求体里写 operator 无效）

## 查看 worker 是否在线

```bash
curl -H "Authorization: Bearer d5521b68d3314b31ba3140f04bff1d1f" \
  https://spark.for-memory.site:38788/api/web/exec/workers
```

返回 `{"workers":[{"worker_id":"w-228","online":true,"busy":false,...}]}`。
`workers` 为空数组 = 那台机器没开机或 worker 没跑，这时下发会拿 404。

## 下发命令

`POST https://spark.for-memory.site:38788/api/web/exec/run`，**同步**等结果返回。

```jsonc
{
  "worker_id": "w-228",
  "script": "Get-Process | Select-Object -First 5",
  "args": [],            // 可选，传给脚本的 param()
  "cwd": null,           // 可选，工作目录（必须已存在）
  "env": {},             // 可选，追加的环境变量
  "timeout_secs": 60,    // 默认 60，上限 3600
  "stdout_limit_bytes": 1048576,  // 默认 1 MiB，上限 8 MiB
  "stderr_limit_bytes": 1048576
}
```

成功返回：

```jsonc
{
  "state": "completed",        // completed | timed_out | spawn_failed
  "source": "worker",
  "id": "<server 生成的 uuid>",
  "exec": {
    "exit_code": 0,            // timed_out / spawn_failed 时为 null
    "stdout": "...",
    "stderr": "...",
    "stdout_truncated": false, // 超出 limit 时为 true，输出被截断
    "stderr_truncated": false,
    "duration_ms": 203,
    "error": null              // spawn_failed 时是原因
  },
  "reason": null
}
```

## 错误码

| 码 | 含义 | 怎么办 |
|---|---|---|
| 401 | token 错/没带 | 检查 Authorization 头 |
| 404 `worker_not_exec_capable` | worker 不在线或没开 exec | 让机器主人把 worker 跑起来 |
| 409 `worker_busy` | 正在执行别的命令 | **第一期一次只能跑一条**，等上一条返回再发 |
| 409 `worker_offline` | 心跳超时 | 同 404 |
| 413 / 422 | body 超 4MiB / 字段超限 | 缩小脚本或参数 |
| 504 `not_picked_up` | 30s 内没被领取 | 命令**没有**执行，可以安全重试 |
| 502 `unknown` | 已派发但结果没回来 | **命令可能已经执行了，禁止自动重试**，先想办法确认状态 |

## 实操注意

- **一次一条**：并发下发第二条必得 409。串行执行。
- **超时会杀整棵进程树**（`taskkill /T /F`），`state=timed_out`、`exit_code=null`，已产生的部分输出照样返回。长命令记得把 `timeout_secs` 调够。
- **退出码是真的**：脚本里 `exit 3` 就返回 3，未捕获异常返回 1。
- **中文没问题**，脚本和输出都是 UTF-8。`param()` 和 `args` 正常工作。
- **在 Windows 的 Git Bash 里用 curl 时**，`-d '{...}'` 容易被 shell 搞坏 JSON（报 `invalid unicode code point`）。把 body 写进文件再 `--data-binary @body.json` 就稳。
- **每次执行两端都会审计**（operator、脚本 SM3 哈希、状态、耗时、输出字节数），但**不记录脚本正文和输出正文**。
- **没有远程取消**：命令发出去就只能等超时。要立即停只能让那台机器的主人在 worker 前台按 Ctrl+C。
- 破坏性操作（删文件、改配置、装卸软件、重启）先跟人确认，这是别人的真实机器。

## 例子

```bash
cat > body.json <<'EOF'
{"worker_id":"w-228","script":"Get-CimInstance Win32_LogicalDisk | Select-Object DeviceID,@{n='FreeGB';e={[math]::Round($_.FreeSpace/1GB,1)}} | Format-Table | Out-String","timeout_secs":30}
EOF
curl -s -X POST https://spark.for-memory.site:38788/api/web/exec/run \
  -H "Authorization: Bearer d5521b68d3314b31ba3140f04bff1d1f" \
  -H "content-type: application/json" --data-binary @body.json
```