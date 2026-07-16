; net-policy 一体化安装钩子（Tauri 2 NSIS installerHooks）。
; 目标：装 GUI 的同时全自动装好 agent（LocalSystem Windows 服务，开机自启 / Session 0 常驻）+ mihomo，
; 并让 GUI 随用户登录自启，全程不烦用户。
; 资源 net-policy-agent.exe / net-policy.exe / mihomo-windows-amd64.exe 由 tauri.conf.json 的
; bundle.resources 打进包，安装时解压到 $INSTDIR。

; 覆盖安装/卸载前的统一停机序列。必须在 Tauri 复制或删除文件之前执行：
; 只停控制面 GUI/agent，**绝不停止 mihomo、绝不撤防火墙**。agent 服务的 Stop 语义本来就是让
; 数据面作为孤儿继续运行，新 agent 启动后会从 generated secret 接管，从而保持严格策略连续性。
!macro NET_POLICY_STOP_FOR_MAINTENANCE PREFIX
  DetailPrint "暂停网络策略控制面（mihomo 与防火墙保持运行）..."
  nsExec::ExecToLog 'taskkill /f /im net-policy-gui.exe'
  Pop $0
  ; net stop 会等待服务进入 Stopped；service 控制处理器只退出 agent，自身不拆 TUN、不停 mihomo、
  ; 不改防火墙。随后仅清理 agent 残留，禁止 taskkill mihomo。
  nsExec::ExecToLog 'net.exe stop net-policy-agent /y'
  Pop $0
  nsExec::ExecToLog 'taskkill /f /im net-policy-agent.exe'
  Pop $0
  Sleep 500
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Tauri 保证本钩子发生在复制 bundle 文件之前，因此旧 exe 尚未被覆盖。
  !insertmacro NET_POLICY_STOP_FOR_MAINTENANCE "preinstall"
!macroend

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "配置网络策略服务（agent + mihomo，开机自启）..."
  ; agent install：把 agent + mihomo 复制到 %ProgramFiles%\net-policy\，并注册为 Windows 服务
  ; （LocalSystem，开机自启 / Session 0 常驻 / 崩溃自动重启），随即启动。安装程序 perMachine 已提权。
  nsExec::ExecToLog '"$INSTDIR\net-policy-agent.exe" install --mihomo "$INSTDIR\mihomo-windows-amd64.pending.exe"'
  Pop $0
  StrCmp $0 "0" net_policy_agent_install_ok
  MessageBox MB_ICONSTOP|MB_OK "net-policy-agent 安装或启动失败（返回码 $0）。请在安装窗口点击 Show details 查看 agent 输出；服务运行日志位于 C:\ProgramData\net-policy\log。"
  Abort
net_policy_agent_install_ok:
  DetailPrint "net-policy-agent install 返回码: $0（Windows 服务及命名管道已就绪）"
  ; L4 抓包明文引擎（mitmproxy）部署：下载 + SHA-256 校验 + Defender 放行 + 解压到
  ; %ProgramFiles%\net-policy\mitm\engine\<ver>\。**best-effort**——L4 是独立高风险可选能力，
  ; 失败只提示、不 Abort（核心网络策略已装好）。离线分发：把 mitmproxy-12.2.3-windows-x86_64.zip
  ; 放进 bundle-resources 并改成 install-mitm-engine --zip "$INSTDIR\mitmproxy-12.2.3-windows-x86_64.zip"。
  DetailPrint "部署 L4 抓包明文引擎（mitmproxy，可选，需联网下载）..."
  nsExec::ExecToLog '"$INSTDIR\net-policy-agent.exe" install-mitm-engine'
  Pop $0
  DetailPrint "install-mitm-engine 返回码: $0（0=已部署；非0=跳过，L4 明文暂不可用，可日后在应用内重试）"
  ; GUI 随用户登录自启：写 HKLM Run 键。GUI 是瘦客户端，管道 ACL 已放行交互用户(IU)、无需提权即可连服务；
  ; Tauri 默认 asInvoker（非提权），故 Run 键能正常拉起。
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Run" "net-policy-gui" '"$INSTDIR\net-policy-gui.exe"'
  DetailPrint "已设置 GUI 登录自启"
  Exec '"$INSTDIR\net-policy-gui.exe"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "停止并移除网络策略服务..."
  ; 真正卸载与覆盖升级不同：用户明确要求移除产品，必须先显式停策略并恢复防火墙基线。
  nsExec::ExecToLog '"$INSTDIR\net-policy.exe" stop'
  Pop $0
  !insertmacro NET_POLICY_STOP_FOR_MAINTENANCE "preuninstall"
  ; 删除服务（下次开机不再自启）。
  nsExec::ExecToLog '"$INSTDIR\net-policy-agent.exe" uninstall'
  Pop $0
  ; 移除 GUI 登录自启键。
  DeleteRegValue HKLM "Software\Microsoft\Windows\CurrentVersion\Run" "net-policy-gui"
!macroend
