//! L4 MITM 引擎（mitmproxy）部署（抓包设计 §17–§18；ADR adr-2026-07-phase4-mitm-engine.md）。
//!
//! GUI 安装程序调用此处把 mitmproxy 引擎落到受保护安装目录，供 agent 后续（Phase 4a 通过后）以
//! sidecar 方式拉起。**本模块只做“把可校验的引擎放到位 + 让 Defender 放行”**，不启动解密、不装 CA、
//! 不改路由——那些是 agent 运行期的高风险步骤，仍受真机 spike 阻断。
//!
//! 关键约束（ADR §4）：
//! - **版本 + SHA-256 锁定**：只接受 [`ENGINE_SHA256`]；不匹配即拒绝，不静默继续（供应链完整性）。
//! - **离线优先**：给了 `zip_src`（GUI 随包预置）就用本地 zip；否则从 [`ENGINE_URL`] 下载（个人项目
//!   允许联网获取；企业分发应预置）。
//! - **Defender 放行**：mitmproxy PyInstaller 二进制会被 Windows Defender 查杀（真机实测，ADR §6），
//!   故部署时对引擎目录加 `Add-MpPreference -ExclusionPath`。这是**修改安全设置**——仅在管理员安装
//!   上下文、对本产品引擎目录、经用户同意（安装即同意，个人项目）执行；best-effort，Defender 缺失/
//!   第三方 AV 时告警但继续。
//! - **受保护位置**：解压到 `%ProgramFiles%\net-policy\mitm\engine\<version>\`（普通用户只读，D6）。
//!
//! 幂等：引擎已就位（`mitmdump.exe` + `.deployed` 标记齐全）则跳过。

use crate::paths;
use crate::win::run_ps_timeout;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

/// 锁定版本（改版须同步改 SHA + 重跑 §18 验收，见 ADR）。
pub const ENGINE_VERSION: &str = "12.2.3";
/// 官方自包含 Windows zip（内置 CPython + OpenSSL）。
pub const ENGINE_URL: &str =
    "https://downloads.mitmproxy.org/12.2.3/mitmproxy-12.2.3-windows-x86_64.zip";
/// 资产 SHA-256（0.228 真机实测，大写 hex；ADR §6）。
pub const ENGINE_SHA256: &str = "04A01EA95AE96DF75058A893E774957D294E69012DAB1F4E256CE2B0C6725483";
/// 无头执行体（agent sidecar 用）。
pub const ENGINE_DUMP_EXE: &str = "mitmdump.exe";

/// 部署超时：下载 ~82MB + 解压可能远超默认 120s，给 12 分钟。
const DEPLOY_TIMEOUT: Duration = Duration::from_secs(720);

/// 引擎目录 `%ProgramFiles%\net-policy\mitm\engine\12.2.3\`。
pub fn engine_dir() -> PathBuf {
    paths::mitm_engine_dir(ENGINE_VERSION)
}

/// `mitmdump.exe` 绝对路径。
pub fn dump_exe() -> PathBuf {
    engine_dir().join(ENGINE_DUMP_EXE)
}

/// 是否已部署（可执行体在位）。
pub fn is_deployed() -> bool {
    dump_exe().exists()
}

/// 部署状态摘要（GUI 展示 / install JSON 内嵌）。
pub fn status() -> Value {
    json!({
        "version": ENGINE_VERSION,
        "deployed": is_deployed(),
        "engine_dir": engine_dir().to_string_lossy(),
        "dump_exe": dump_exe().to_string_lossy(),
    })
}

/// PS 单引号转义（`'` → `''`），防路径里的引号截断字符串。
fn ps_quote(s: &str) -> String {
    s.replace('\'', "''")
}

/// 部署引擎。`zip_src` 为本地预置 zip 路径（离线安装）；`None` 则从官方站下载。
/// 须管理员（写 ProgramFiles + 改 Defender 排除）。返回结构化结果。
pub fn deploy(zip_src: Option<&str>) -> Result<Value> {
    if !crate::win::is_windows() {
        bail!("仅支持 Windows");
    }
    if !crate::win::is_elevated() {
        bail!("部署 MITM 引擎需要管理员权限（写 %ProgramFiles% + 配置 Defender 排除）。");
    }
    let dir = engine_dir();
    let dir_s = ps_quote(&dir.to_string_lossy());
    let excl_root = ps_quote(&paths::mitm_engine_root().to_string_lossy());
    let zip_s = ps_quote(zip_src.unwrap_or(""));
    let url = ENGINE_URL; // 常量，无注入面
    let sha = ENGINE_SHA256; // 常量
    let dump = ENGINE_DUMP_EXE;

    // 注：脚本内所有插值来源——dir/excl_root 由 env 派生的固定路径，zip_s 经单引号转义，url/sha 为
    // 编译期常量。Defender 排除、下载、SHA 校验、解压、标记全在提权 PS 内完成，输出结构化标签。
    let script = format!(
        r#"$engineDir = '{dir_s}'
$exclRoot  = '{excl_root}'
$zipSrc    = '{zip_s}'
$url       = '{url}'
$expected  = '{sha}'.ToUpper()
$dumpExe   = Join-Path $engineDir '{dump}'
$marker    = Join-Path $engineDir '.deployed'

if ((Test-Path $dumpExe) -and (Test-Path $marker)) {{ 'STATUS=already'; return }}

# 1) Defender 放行（best-effort；先于解压，避免 mitmdump.exe 落盘即被查杀）
try {{
    Add-MpPreference -ExclusionPath $exclRoot -ErrorAction Stop
    'DEFENDER=excluded'
}} catch {{ 'DEFENDER=skip:' + $_.Exception.Message.Replace([Environment]::NewLine,' ') }}

New-Item -ItemType Directory -Force -Path $engineDir | Out-Null

# 2) 取 zip：本地预置优先，否则下载
$tmp = Join-Path $env:TEMP ('mitmproxy-' + [guid]::NewGuid().ToString() + '.zip')
if ($zipSrc -and (Test-Path $zipSrc)) {{
    Copy-Item -LiteralPath $zipSrc -Destination $tmp -Force
    'SOURCE=local'
}} else {{
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $tmp -TimeoutSec 600
    'SOURCE=download'
}}

# 3) SHA-256 完整性校验（不匹配即拒绝）
$sha = (Get-FileHash -Algorithm SHA256 -LiteralPath $tmp).Hash.ToUpper()
'SHA256=' + $sha
if ($sha -ne $expected) {{
    Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    throw ('SHA-256 校验失败：期望 ' + $expected + ' 实得 ' + $sha)
}}

# 4) 清目录旧内容后解压（保证干净版本）
Get-ChildItem -LiteralPath $engineDir -Force -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Expand-Archive -LiteralPath $tmp -DestinationPath $engineDir -Force
Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue

if (-not (Test-Path $dumpExe)) {{ throw '解压后未见 {dump}（可能仍被 AV 拦截）' }}
Set-Content -LiteralPath $marker -Value ('{ENGINE_VERSION} ' + (Get-Date -Format o)) -Encoding utf8
'STATUS=deployed'
"#
    );

    let out = run_ps_timeout(&script, DEPLOY_TIMEOUT).context("部署 MITM 引擎失败")?;
    let deployed = out.contains("STATUS=deployed") || out.contains("STATUS=already");
    let already = out.contains("STATUS=already");
    if !deployed || !dump_exe().exists() {
        bail!("引擎部署未完成：{}", out.trim());
    }
    Ok(json!({
        "result": if already { "already_deployed" } else { "deployed" },
        "version": ENGINE_VERSION,
        "dump_exe": dump_exe().to_string_lossy(),
        "defender_excluded": out.contains("DEFENDER=excluded"),
        "source": if out.contains("SOURCE=local") { "local" } else if out.contains("SOURCE=download") { "download" } else { "cached" },
        "detail": out.trim(),
    }))
}

/// 卸载清理（ADR §5）：撤销 Defender 排除 + 删除引擎目录。best-effort——返回是否有实际删除，
/// 失败只记录不上抛（卸载主流程不能因引擎残留而中断）。须管理员。
pub fn cleanup() -> Value {
    if !crate::win::is_windows() || !crate::win::is_elevated() {
        return json!({"result": "skipped", "reason": "非 Windows 或未提权"});
    }
    let root = engine_dir().parent().map(|p| p.to_path_buf());
    let Some(root) = root else {
        return json!({"result": "skipped", "reason": "无引擎根目录"});
    };
    let root_s = ps_quote(&root.to_string_lossy());
    let script = format!(
        r#"$root = '{root_s}'
try {{ Remove-MpPreference -ExclusionPath $root -ErrorAction Stop; 'DEFENDER=removed' }} catch {{ 'DEFENDER=skip' }}
if (Test-Path $root) {{ Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue; 'ENGINE=removed' }} else {{ 'ENGINE=absent' }}
"#
    );
    match run_ps_timeout(&script, Duration::from_secs(60)) {
        Ok(out) => json!({
            "result": "cleaned",
            "defender_exclusion_removed": out.contains("DEFENDER=removed"),
            "engine_removed": out.contains("ENGINE=removed"),
        }),
        Err(e) => json!({"result": "cleanup_failed", "error": e.to_string()}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha_is_uppercase_hex_64() {
        assert_eq!(ENGINE_SHA256.len(), 64);
        assert!(ENGINE_SHA256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)));
    }

    #[test]
    fn engine_paths_under_install_dir() {
        // 引擎必须落在受保护安装目录内（D6）。
        let root = paths::install_dir();
        assert!(engine_dir().starts_with(&root));
        assert!(dump_exe().starts_with(&root));
        assert!(dump_exe().ends_with("mitmdump.exe"));
    }

    #[test]
    fn ps_quote_escapes_single_quotes() {
        assert_eq!(ps_quote(r"C:\a'b"), "C:\\a''b");
        assert_eq!(ps_quote(""), "");
    }

    #[test]
    fn status_shape() {
        let s = status();
        assert_eq!(s["version"], ENGINE_VERSION);
        assert!(s["deployed"].is_boolean());
    }
}
