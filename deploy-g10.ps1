#requires -Version 7
<#
.SYNOPSIS
    交叉编译 zero 工具集（aarch64-linux）并部署到 G10 设备。

.DESCRIPTION
    Windows 开发机 → 用预置交叉编译镜像在 Docker 容器内构建 aarch64-unknown-linux-gnu
    的 release 二进制（带 prod feature，日志落文件、stdout 保持 JSON 干净）→ scp 到 G10。

    镜像 huangjiemin/rust_aarch64-gcc_openssl 已预置：aarch64 gcc/ar/g++、cmake、clang，
    以及 CARGO_TARGET_*_LINKER / CC_/CXX_ 环境变量（aws-lc-sys 的 C 交叉所需）。

.PARAMETER G10Host
    G10 的 ssh 目标，默认 fengqi@192.168.0.68。

.PARAMETER SshPort
    G10 的 ssh 端口，默认 22（局域网）。外网走 caddy 旁的 SSH 映射时传 `-SshPort 2222`
    并把 `-G10Host` 换成公网域名，例：
    `pwsh ./deploy-g10.ps1 -G10Host fengqi@spark.for-memory.site -SshPort 2222`。

.PARAMETER DestDir
    G10 上的安装目录，默认 ~/.local/bin（与 custom-utils updater 自更新目标一致）。

.PARAMETER SkipBuild
    跳过交叉编译，直接复制已有产物（调试部署用）。

.PARAMETER Service
    部署后要 install + 重启的 systemd 用户服务名,默认 toolkit-server。daemon 见 $DaemonBins。
    二进制名即服务名。其余 $Bins 是 CLI 工具,无需重启。部署面板按各服务分别调用本脚本
    (每次一个 -Service)。
    注:orchestrator(ASR 编排层)已并入 toolkit-server 同进程(ASR 走 /api/asr/stream),
    不再单独部署;G10 上若残留旧 orchestrator 服务,执行
    `systemctl --user disable --now orchestrator` 停掉(其 app.db 可拷到 toolkit-server
    workspace 以保留声纹/历史)。

.PARAMETER SkipRestart
    跳过部署后重启（仅换二进制，下次服务自然重启时生效）。

.PARAMETER Bind
    toolkit-server 的监听地址，默认 0.0.0.0:8788。部署时通过 `toolkit-server install`
    把它写进 systemd unit 的 `Environment=TOOLKIT_BIND=<Bind>`（重装 unit 使新端口生效）。
    G10 部署面板会把该服务 registry 主端口拼成 `0.0.0.0:<port>` 传进来。

.PARAMETER Workspace
    daemon 的 workspace 根目录（远端路径）。留空则按服务名取 `~/.config/<Service>`
    （与各 daemon install 默认一致）。install 时显式传给 `--workspace`。

.PARAMETER Env
    额外注入 systemd unit 的环境变量，`KEY=VAL` 数组（逗号分隔）。install 时逐条转发为
    `--env KEY=VAL`（custom-utils 0.16 写进 unit 的 `Environment=`），键冲突时覆盖内置默认
    （含 `--bind` 的 `TOOLKIT_BIND`）。G10 部署面板按各服务配置的环境变量传入。
    例：`-Env TTS_BASE_URL=http://127.0.0.1:8095,LLM_BASE_URL=http://127.0.0.1:8000/v1`。

.EXAMPLE
    pwsh ./deploy-g10.ps1
    pwsh ./deploy-g10.ps1 -SkipBuild
    pwsh ./deploy-g10.ps1 -SkipRestart
    pwsh ./deploy-g10.ps1 -Bind 0.0.0.0:8790
    pwsh ./deploy-g10.ps1 -Env TTS_BASE_URL=http://127.0.0.1:8095,LLM_MODEL=qwen
#>
param(
    [string]$G10Host = "fengqi@192.168.0.68",
    [int]$SshPort = 22,
    [string]$DestDir = "~/.local/bin",
    [string]$Service = "toolkit-server",
    [string]$Bind = "0.0.0.0:8788",
    # 远端 workspace 根目录;留空则按服务名取 `~/.config/<Service>`(与各 daemon install 默认一致)。
    [string]$Workspace = "",
    [string[]]$Env = @(),
    [switch]$SkipBuild,
    [switch]$SkipRestart
)

$ErrorActionPreference = "Stop"
$RepoRoot = $PSScriptRoot
# ssh 与 scp 的端口开关拼法不同（小写 -p / 大写 -P），统一在此备好，下面逐处 splat。
$SshPortArgs = @("-p", "$SshPort")
$ScpPortArgs = @("-P", "$SshPort")
$Target = "aarch64-unknown-linux-gnu"
$Image = "huangjiemin/rust_aarch64-gcc_openssl:1.94.0_9.4.0_1.1.0l_llvm12.0.1"

# (crate package 名, 产物二进制名) —— 新增工具时在此追加一行即可。
$Bins = @(
    @{ Crate = "github_commit_info"; Bin = "github-commit-info" },
    @{ Crate = "hf_watcher";         Bin = "hf-watcher" },
    @{ Crate = "douyin";             Bin = "douyin" },
    @{ Crate = "toolkit-server";     Bin = "toolkit-server" },
    # 出口代理节点(借出口执行端)。CLI 型:无 systemd unit,不进 $DaemonBins;
    # 各出口机上手动/自更新拉起 `toolkit-worker --controller <公网> --token <...>`。
    @{ Crate = "toolkit-worker";     Bin = "toolkit-worker" }
)
# 注:orchestrator 已并入 toolkit-server 同进程(ASR=/api/asr/stream),不再单独构建/部署。
# 其 ASR 下游地址走环境变量(ASR_WS/ASR_EMBED/VLLM_BASE/VLLM_MODEL),缺省即本机回环,
# 需要时经 -Env 注入 toolkit-server unit。

# 哪些 $Bins 是 daemon（有 `install`/systemd unit、需重装+重启）；其余是 CLI 工具。
# 二进制名即 unit/服务名。新增 daemon 时在此追加。
$DaemonBins = @("toolkit-server")

# 产物输出目录（host 可见，从容器内的 CARGO_TARGET_DIR 拷出来）。
# 改用 dist/g10 而非 target/ 是为了把 CARGO_TARGET_DIR 放进命名卷（Linux ext4），
# 避免 Windows NTFS 经 Docker Desktop 的 mtime/权限抖动让 cargo 指纹失效每次全量重编。
$OutDir = Join-Path $RepoRoot "dist/g10"

if (-not $SkipBuild) {
    Write-Host "==> 交叉编译 $Target（Docker: $Image）" -ForegroundColor Cyan
    if (-not (Get-Command docker -ErrorAction SilentlyContinue)) {
        throw "未找到 docker，请先安装/启动 Docker Desktop。"
    }

    New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

    # 单次 cargo build 串多个 -p，让 cargo 跨 crate 调度依赖图（比逐 crate 串行更快）。
    $pkgArgs = ($Bins | ForEach-Object { "-p $($_.Crate)" }) -join " "
    $copyCmd = ($Bins | ForEach-Object {
        "cp /cargo-target/$Target/release/$($_.Bin) /work/dist/g10/"
    }) -join " && "
    $buildCmd = "cargo build --release --target $Target $pkgArgs --features prod && " `
        + "mkdir -p /work/dist/g10 && $copyCmd"

    # workspace Cargo.toml 当前用本地 path 依赖 `custom-utils = { path = "../custom-utils" }`，
    # 容器内仓库挂在 /work，故 ../custom-utils 解析为 /custom-utils —— 必须把同级目录也挂进去。
    $CustomUtils = Join-Path (Split-Path $RepoRoot -Parent) "custom-utils"
    if (-not (Test-Path (Join-Path $CustomUtils "Cargo.toml"))) {
        throw "未找到本地 custom-utils（$CustomUtils）；workspace 依赖 path = ../custom-utils，无它无法交叉编译。"
    }

    # 命名卷缓存(挂对镜像实际用的路径,不依赖任何 env 假设):
    #   - shared-cargo-registry → /root/.cargo/registry。
    #     镜像里 `CARGO_HOME` 未设、cargo 默认走 `$HOME/.cargo = /root/.cargo`(已 docker exec
    #     进去 echo 确认过);**不是** `/usr/local/cargo`——之前挂在那等于挂了个空目录,cargo 仍
    #     把 crate 下载到 /root/.cargo/registry(容器临时层),退出就丢,每次都全量下载。
    #     registry 跨项目可共享(crate 按 name-version 寻址,不冲突)。
    #   - toolkit-cargo-target → /cargo-target。编译产物 + fingerprint,**必须项目专属**(不同
    #     workspace 共用 target 会互相覆盖指纹 → 每次全量重编)。
    # 命名卷在 Linux ext4 上,避免 NTFS 经 Docker Desktop 时 mtime 抖动让 cargo 指纹失效。
    # 仓库 + 同级 custom-utils 仍 bind-mount(源码必须 host 可写);产物在容器内 cp 到 /work/dist/g10。
    # AR_ 显式补上(镜像只预置了 CC_/CXX_/LINKER)。
    docker run --rm `
        -v "${RepoRoot}:/work" `
        -v "${CustomUtils}:/custom-utils" `
        -v "shared-cargo-registry:/root/.cargo/registry" `
        -v "toolkit-cargo-target:/cargo-target" `
        -w /work `
        -e CARGO_TARGET_DIR=/cargo-target `
        -e AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar `
        $Image bash -lc $buildCmd
    if ($LASTEXITCODE -ne 0) { throw "交叉编译失败（exit $LASTEXITCODE）" }
}

# 校验产物存在（统一在 dist/g10 下；-SkipBuild 时也读这里）。
foreach ($b in $Bins) {
    $p = Join-Path $OutDir $b.Bin
    if (-not (Test-Path $p)) { throw "产物缺失：$p（先去掉 -SkipBuild 完整构建）" }
}
$ReleaseDir = $OutDir

Write-Host "==> 部署到 ${G10Host}:${DestDir}（ssh 端口 $SshPort）" -ForegroundColor Cyan
# 确保远端目录存在。
ssh @SshPortArgs $G10Host "mkdir -p $DestDir"
if ($LASTEXITCODE -ne 0) { throw "无法在 G10 创建目录 $DestDir（检查 ssh 连通性）" }

foreach ($b in $Bins) {
    $local = Join-Path $ReleaseDir $b.Bin
    $dest = "$DestDir/$($b.Bin)"
    Write-Host "    scp $($b.Bin)"
    # 先传到 .new 临时名，再 mv 覆盖：rename 即使旧二进制正在运行也能替换
    # （直接 scp 覆盖运行中的二进制会 ETXTBSY / dest open Failure）。
    scp @ScpPortArgs $local "${G10Host}:${dest}.new"
    if ($LASTEXITCODE -ne 0) { throw "scp $($b.Bin) 失败" }
    ssh @SshPortArgs $G10Host "chmod +x ${dest}.new && mv -f ${dest}.new ${dest}"
    if ($LASTEXITCODE -ne 0) { throw "替换 $($b.Bin) 失败（mv）" }
}

# 打印版本确认。
Write-Host "==> 远端版本确认" -ForegroundColor Cyan
foreach ($b in $Bins) {
    ssh @SshPortArgs $G10Host "$DestDir/$($b.Bin) --version"
}

# 重装 daemon unit：把 -Bind 写进 unit 的 Environment=<BIN>_BIND=<Bind>（各 daemon 的
# install 子命令各自决定 env 名），使新端口生效（install 幂等：重写 unit + daemon-reload）。
# 仅 $DaemonBins 里的服务支持 install,其余 $Bins 是 CLI 工具、无 unit、跳过。
# toolkit-server 与 orchestrator 的 install CLI 一致(`install --workspace --bind --env`),
# 故此处按 $Service 通用处理,二进制名即服务名。
if ($DaemonBins -contains $Service) {
    # workspace 留空则按服务名取默认,与各 daemon install 的 `~/.config/<Service>` 一致。
    $ws = if ($Workspace) { $Workspace } else { "~/.config/$Service" }
    Write-Host "==> 重装 $Service unit（bind=$Bind, workspace=$ws）" -ForegroundColor Cyan
    # 把每条 KEY=VAL 拼成 `--env 'KEY=VAL'`（单引号防远端 shell 二次解析），追加进 install 命令。
    # 注：面板把多条 env 拼成 "K1=V1,K2=V2,K3=V3" 作单参传入（`[string[]]` 从 `pwsh -File`
    # 单 argv 不会自动拆逗号），故先按逗号展开再逐条转 `--env`。
    $envArgs = ($Env | Where-Object { $_ -and $_.Trim() -ne "" } | ForEach-Object {
        $_.Split(",") | Where-Object { $_.Trim() -ne "" } | ForEach-Object { "--env '$($_.Trim())'" }
    }) -join " "
    if ($envArgs) {
        Write-Host "    注入环境变量：$($Env -join ', ')" -ForegroundColor DarkGray
    }
    $installCmd = 'export XDG_RUNTIME_DIR=/run/user/$(id -u); ' `
        + "$DestDir/$Service install --workspace $ws --bind $Bind $envArgs"
    ssh @SshPortArgs $G10Host $installCmd
    if ($LASTEXITCODE -ne 0) { throw "$Service install 失败（重装 unit）" }
}

# 重启 daemon 用户服务（CLI 工具无服务、不涉及；换二进制后服务需重启才加载新版）。
# XDG_RUNTIME_DIR 显式补上：非交互 ssh 默认不带，systemctl --user 会找不到 user bus。
if (-not $SkipRestart) {
    Write-Host "==> 重启 $Service" -ForegroundColor Cyan
    $restartCmd = 'export XDG_RUNTIME_DIR=/run/user/$(id -u); ' `
        + "systemctl --user restart $Service && " `
        + "sleep 2 && " `
        + "systemctl --user is-active $Service && " `
        + "systemctl --user status $Service --no-pager -n 5"
    ssh @SshPortArgs $G10Host $restartCmd
    if ($LASTEXITCODE -ne 0) { throw "重启 $Service 失败（检查服务名 / 是否已 systemctl --user enable）" }
} else {
    Write-Host "==> 跳过重启（-SkipRestart）" -ForegroundColor DarkGray
}

Write-Host "==> 完成。若 $DestDir 不在 G10 的 PATH，请确认 zero 能按该路径调用工具。" -ForegroundColor Green
