<#
.SYNOPSIS
  打包 net-policy 一体化 NSIS 安装程序：GUI + agent + mihomo 装到一个 setup.exe，
  装完 agent 以 LocalSystem Windows 服务开机自启，全自动、不烦用户。

.DESCRIPTION
  流程：
    1. 编译 net-policy-agent（release + prod：日志落文件）与 net-policy（救援 CLI，release）；
    2. 把两个 exe + mihomo 拷进 crates/net-policy-gui/bundle-resources/（tauri.conf.json 的
       bundle.resources 会把它们打进 NSIS 包）；
    3. cargo tauri build 打出 NSIS setup.exe（内含 installer-hooks.nsh：POSTINSTALL 自动跑
       `net-policy-agent install` 注册开机自启 + 立即启动；PREUNINSTALL 优雅停+撤服务）。

.PARAMETER Mihomo
  mihomo-windows-amd64.exe 的路径。缺省取 zero-desktop 目录下那份。

.EXAMPLE
  pwsh ./package-net-policy.ps1
  pwsh ./package-net-policy.ps1 -Mihomo D:\bins\mihomo-windows-amd64.exe
#>
param(
  [string]$Mihomo = "$env:USERPROFILE\.config\zero-desktop\net-policy\mihomo-windows-amd64.exe"
)
$ErrorActionPreference = "Stop"
$repo = $PSScriptRoot
$gui  = Join-Path $repo "crates\net-policy-gui"
$res  = Join-Path $gui  "bundle-resources"

Write-Host "== 1/4 编译 agent(release+prod) + 救援 CLI(release) ==" -ForegroundColor Cyan
cargo build --release -p net-policy-agent --features prod
if ($LASTEXITCODE -ne 0) { throw "net-policy-agent release 构建失败（退出码 $LASTEXITCODE）" }
cargo build --release -p net-policy-cli
if ($LASTEXITCODE -ne 0) { throw "net-policy-cli release 构建失败（退出码 $LASTEXITCODE）" }

Write-Host "== 2/4 收集资源到 bundle-resources ==" -ForegroundColor Cyan
New-Item -ItemType Directory -Force $res | Out-Null
Copy-Item "$repo\target\release\net-policy-agent.exe" "$res\net-policy-agent.exe" -Force
Copy-Item "$repo\target\release\net-policy.exe"       "$res\net-policy.exe"       -Force
if (-not (Test-Path $Mihomo)) {
  throw "找不到 mihomo：$Mihomo —— 用 -Mihomo <路径> 指定 mihomo-windows-amd64.exe"
}
Copy-Item $Mihomo "$res\mihomo-windows-amd64.exe" -Force
Write-Host "  agent / net-policy / mihomo 已就位"

Write-Host "== 3/4 cargo tauri build（打 NSIS，自动先 npm run build 前端）==" -ForegroundColor Cyan
Push-Location $gui
try {
  cargo tauri build
  if ($LASTEXITCODE -ne 0) { throw "cargo tauri build 失败（退出码 $LASTEXITCODE）" }
} finally { Pop-Location }

Write-Host "== 4/4 产物 ==" -ForegroundColor Green
Get-ChildItem "$repo\target\release\bundle\nsis\*.exe" -ErrorAction SilentlyContinue |
  ForEach-Object { Write-Host "  $($_.FullName)" -ForegroundColor Green }
Write-Host "完成：双击该 setup.exe 即可一键装好 GUI+agent+mihomo，agent 服务开机自启。"
