//! 直接用 Windows IP Helper API 读取系统路由表。
//!
//! 这是 GUI 的只读诊断能力，不经过 net-policy-agent：即使 agent 未安装、未启动或网络策略未启用，
//! 也能看到当前 Windows 实际生效的 IPv4/IPv6 路由。
//!
//! 实现走原生 FFI（`GetIpForwardTable2` + `ConvertInterfaceLuidToAlias` + `GetIpInterfaceEntry`），
//! **不再拉起 PowerShell**：无子进程、无控制台闪窗、无 `Get-NetRoute` 输出经 OEM 代码页时把中文
//! 网卡别名解成乱码的问题（宽字符 API 直接给 UTF-16）。

use anyhow::{bail, Result};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct SystemRoute {
    pub destination_prefix: String,
    pub next_hop: String,
    pub interface_alias: String,
    pub interface_index: u32,
    pub route_metric: u32,
    pub interface_metric: u32,
    pub protocol: String,
    pub state: String,
    pub address_family: String,
}

/// 不需要管理员权限的只读系统路由快照。
#[cfg(windows)]
pub fn read() -> Result<Vec<SystemRoute>> {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        ConvertInterfaceLuidToAlias, FreeMibTable, GetIpForwardTable2, GetIpInterfaceEntry,
        InitializeIpInterfaceEntry, MIB_IPFORWARD_TABLE2, MIB_IPINTERFACE_ROW,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
    use windows_sys::Win32::Networking::WinSock::{
        ADDRESS_FAMILY, AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_INET,
    };

    // SOCKADDR_INET（IPv4/IPv6 联合体）→ IpAddr；未知地址族返回 None。
    // SAFETY: 调用者保证 `sa` 指向有效的 SOCKADDR_INET；按 si_family 读对应分支的联合体成员。
    unsafe fn sockaddr_ip(sa: &SOCKADDR_INET) -> Option<IpAddr> {
        match sa.si_family {
            // S_addr 的内存字节即网络序（大端）四段，to_ne_bytes 拿到的就是点分顺序，端序无关。
            AF_INET => Some(IpAddr::V4(Ipv4Addr::from(
                sa.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes(),
            ))),
            AF_INET6 => Some(IpAddr::V6(Ipv6Addr::from(sa.Ipv6.sin6_addr.u.Byte))),
            _ => None,
        }
    }

    // 网卡友好名（如「以太网」「WLAN」）：宽字符 API，直接得 UTF-16，无代码页转换。
    fn iface_alias(luid: NET_LUID_LH) -> String {
        // NDIS_IF_MAX_STRING_SIZE(256) + 1 结尾 NUL。
        let mut buf = [0u16; 257];
        // SAFETY: 缓冲区按 ConvertInterfaceLuidToAlias 契约定长；失败时保持全零。
        let ret = unsafe { ConvertInterfaceLuidToAlias(&luid, buf.as_mut_ptr(), buf.len()) };
        if ret != ERROR_SUCCESS {
            return String::new();
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    // 接口级 Metric（Get-NetRoute 的 InterfaceMetric）：按 (LUID, 地址族) 缓存，避免逐行重复系统调用。
    fn iface_metric(
        cache: &mut HashMap<(u64, ADDRESS_FAMILY), u32>,
        luid: NET_LUID_LH,
        family: ADDRESS_FAMILY,
    ) -> u32 {
        // SAFETY: NET_LUID_LH 是 Copy 联合体，读 Value（u64）视图仅取其位模式作缓存键。
        let key = (unsafe { luid.Value }, family);
        if let Some(&m) = cache.get(&key) {
            return m;
        }
        // SAFETY: 先零初始化再由 InitializeIpInterfaceEntry 按契约填默认，随后指定 Family/LUID 查询。
        let metric = unsafe {
            let mut row: MIB_IPINTERFACE_ROW = std::mem::zeroed();
            InitializeIpInterfaceEntry(&mut row);
            row.Family = family;
            row.InterfaceLuid = luid;
            if GetIpInterfaceEntry(&mut row) == ERROR_SUCCESS {
                row.Metric
            } else {
                0
            }
        };
        cache.insert(key, metric);
        metric
    }

    let mut table_ptr: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
    // SAFETY: 由系统分配整表，下方 FreeMibTable 释放；AF_UNSPEC 同时取 IPv4+IPv6。
    let ret = unsafe { GetIpForwardTable2(AF_UNSPEC, &mut table_ptr) };
    if ret != ERROR_SUCCESS {
        bail!("GetIpForwardTable2 读取系统路由表失败：错误码 {ret}");
    }
    if table_ptr.is_null() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    let mut metric_cache: HashMap<(u64, ADDRESS_FAMILY), u32> = HashMap::new();
    // SAFETY: table_ptr 在 FreeMibTable 前一直有效；NumEntries 界定 Table 变长数组的行数。
    unsafe {
        let table = &*table_ptr;
        let rows = std::slice::from_raw_parts(table.Table.as_ptr(), table.NumEntries as usize);
        for row in rows {
            let family = row.DestinationPrefix.Prefix.si_family;
            let family_label = match family {
                AF_INET => "IPv4",
                AF_INET6 => "IPv6",
                _ => continue, // 只关心 IPv4/IPv6，其余（如 AF_UNSPEC 占位行）跳过。
            };
            let dest_ip = sockaddr_ip(&row.DestinationPrefix.Prefix)
                .map(|ip| ip.to_string())
                .unwrap_or_default();
            out.push(SystemRoute {
                destination_prefix: format!("{dest_ip}/{}", row.DestinationPrefix.PrefixLength),
                next_hop: sockaddr_ip(&row.NextHop)
                    .map(|ip| ip.to_string())
                    .unwrap_or_default(),
                interface_alias: iface_alias(row.InterfaceLuid),
                interface_index: row.InterfaceIndex,
                route_metric: row.Metric,
                interface_metric: iface_metric(&mut metric_cache, row.InterfaceLuid, family),
                protocol: protocol_name(row.Protocol),
                // 转发表返回的都是已安装、生效中的路由；与 Get-NetRoute 对绝大多数行的展示一致。
                state: "Alive".to_string(),
                address_family: family_label.to_string(),
            });
        }
    }
    // SAFETY: table_ptr 来自 GetIpForwardTable2，必须且只能由 FreeMibTable 释放一次。
    unsafe { FreeMibTable(table_ptr.cast()) };

    // 与旧版 PowerShell（Sort-Object AddressFamily, DestinationPrefix, 总 Metric）保持一致的稳定顺序。
    out.sort_by(|a, b| {
        a.address_family
            .cmp(&b.address_family)
            .then_with(|| a.destination_prefix.cmp(&b.destination_prefix))
            .then_with(|| {
                (a.route_metric + a.interface_metric).cmp(&(b.route_metric + b.interface_metric))
            })
    });
    Ok(out)
}

#[cfg(not(windows))]
pub fn read() -> Result<Vec<SystemRoute>> {
    bail!("系统路由表仅支持 Windows")
}

/// NL_ROUTE_PROTOCOL（i32）→ Get-NetRoute 风格的协议名；未收录的值回退为数字串。
#[cfg(windows)]
fn protocol_name(protocol: i32) -> String {
    let name = match protocol {
        1 => "Other",
        2 => "Local",
        3 => "NetMgmt",
        4 => "Icmp",
        5 => "Egp",
        6 => "Ggp",
        7 => "Hello",
        8 => "Rip",
        9 => "IsIs",
        10 => "EsIs",
        11 => "Cisco",
        12 => "Bbn",
        13 => "Ospf",
        14 => "Bgp",
        10002 => "NtAutostatic",
        10006 => "NtStatic",
        10007 => "NtStaticNonDod",
        other => return other.to_string(),
    };
    name.to_string()
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn reads_live_windows_routes_and_serializes_for_frontend() {
        let routes = super::read().unwrap();
        assert!(!routes.is_empty(), "本机应至少有一条系统路由");
        // 至少应有一条默认路由（0.0.0.0/0 或 ::/0）。
        assert!(
            routes
                .iter()
                .any(|r| r.destination_prefix == "0.0.0.0/0" || r.destination_prefix == "::/0"),
            "未见默认路由，读取疑似不完整"
        );
        let json = serde_json::to_value(&routes[0]).unwrap();
        // Tauri 前端按 snake_case 消费；不得出现 PascalCase 残留。
        assert!(json.get("destination_prefix").is_some());
        assert!(json.get("interface_index").is_some());
        assert!(json.get("DestinationPrefix").is_none());
    }
}
