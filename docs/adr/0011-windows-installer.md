# 0011 Windows 安装包

状态：已采纳

## 背景

原来 Windows 上发的是 CI 组装的一个文件夹：根目录一个 `screenferry.exe`，`settings\` 里是 Flutter 设置界面及其
运行库，散着一堆文件，用户不知道点哪个，也没有开始菜单、卸载入口。另外还有这些问题：

- `screenferry.exe` 是控制台程序（它同时是命令行工具），双击、登录时启动都会闪一下黑色窗口，再自己关掉。
- `screenferry.exe` 没有图标和版本信息；设置界面的文件说明、产品名都是工程名 `screenferry_settings`。
- 两个程序都依赖 Visual C++ 运行库（`VCRUNTIME140.dll` 等），包里没带，没装过运行库的电脑打不开。
- 每次第一次运行都要在防火墙的提示里点允许。

本项目不做代码签名（证书要花钱），所以只能在不签名的前提下把能做的做好。

## 决定

### 安装包

- 用 Inno Setup 7.1 或更高版本（`packaging/windows/screenferry.iss`）。它从 7.1 起自带简体中文界面，不用往仓库里放
  第三方翻译。`cargo xtask bundle` 先组装程序目录，再调用 ISCC 打出
  `ScreenFerry-<版本>-windows-x64-setup.exe`；CI 下载官方发布的 7.1.1 并核对校验和（运行器自带的是 6.x）。
  也考虑过 WiX（MSI）：更适合企业批量部署，但写起来繁琐得多，在 macOS 上也没法检查，暂时用不上。
- 装进 Program Files（`{autopf}\ScreenFerry`），需要管理员权限。所有用户都能用；防火墙规则只有管理员能加；
  以后要做的 Windows 安全桌面服务（`screenferry-service`）也必须装在这里。
- 开始菜单有 ScreenFerry，桌面快捷方式可选；“设置 → 应用”里能卸载，显示图标、版本、发布者。
- 防火墙：给 `screenferry.exe` 加一条入站放行规则（`netsh advfirewall`），只放行专用网络和域网络，覆盖键鼠、剪贴板、
  邀请端口和局域网发现。公用网络不放行，连到公用网络时 Windows 照常询问，由用户决定。卸载时删掉。
- 升级：直接运行新的安装包，`AppId` 不变。安装前先请正在运行的 ScreenFerry 退出（见下面的 `quit`），
  不支持 `quit` 的旧版本再强行结束；`settings\` 整个换新。装完马上以管理员权限重新启动，不弹出设置界面。
  这一点很要紧：用户常常正从另一台电脑控制这台，后台一退出键鼠就断了，等不到点“完成”。
  第一次安装则在完成页上可以选择启动，打开设置界面开始配对。
- 卸载：先 `quit`，再删程序文件、防火墙规则和指向本程序的开机自启登记。配置和配对记录留在 `%APPDATA%`，
  以后重装还能用。
- 开机自启登记在当前用户的“登录时启动、最高权限”任务计划程序任务下，记的是程序的完整路径。
  从解压的文件夹换成安装版后，安装程序和后台启动时都会把它改成现在的位置；旧版本的 `HKCU\...\Run`
  登记会迁移并删除，免得登录时启动的还是旧的那一份。

### 程序本身

- `screenferry.exe` 改成 Windows 子系统程序，双击、登录时启动都没有黑色窗口。命令行用法自己接控制台：
  `peers`、`quit` 等接回启动它的终端（只输出）；`serve`、`connect` 要确认配对码、按 Ctrl+C 退出，和终端抢输入会乱，
  所以新开一个窗口，结束时停一下让人看清输出。
- 用 `build.rs`（`embed-resource`）把 `packaging/windows/screenferry.rc` 编进程序：图标（`assets/icons/app-icon-windows.ico`，
  `cargo xtask icons` 导出，和设置界面用的是同一个）、版本信息（文件说明、产品名都是 ScreenFerry，版本号取自 Cargo.toml），
  以及程序清单 `packaging/windows/app.manifest`：每显示器 DPI 感知、新版系统控件外观、长路径、支持 Windows 10/11。
- 清单里 `uiAccess` 关着。它能让程序操作以管理员身份运行的窗口，但要求程序经过签名并装在受保护的目录里，
  不签名时打开它程序就启动不了。现在由常驻后台在启动时经 UAC 提升来解决管理员窗口，见 ADR 0015。
- C 运行库静态链接（`.cargo/config.toml` 里 MSVC 目标的 `+crt-static`），后台不再依赖 `VCRUNTIME140.dll`。
  设置界面是 Flutter 程序，没法静态链接，按 Flutter 的发布说明把 `msvcp140.dll`、`vcruntime140.dll`、
  `vcruntime140_1.dll` 放在它旁边。
- 设置界面改名为 `screenferry-settings.exe`（符合 `screenferry[-<角色>]` 的命名规则），文件说明是“ScreenFerry 设置”，
  任务管理器里显示的就是它。

### 退出命令

- 新增 `screenferry quit`：经本机通信请正在运行的后台退出，等它真的退出（删掉 `ipc.json`）后返回。
  本机通信新增命令 `quit` 和通知 `quit`：后台退出前先通知所有界面，设置界面随之关闭，不再占着程序文件。
  托盘里的“退出”也一样，设置界面跟着关掉。

## 不做

- 代码签名。没有签名时第一次运行会被 SmartScreen 拦一下，要点“更多信息 → 仍要运行”；以后有了证书，
  Inno Setup 的 SignTool 和 `uiAccess` 都可以接着做。
- MSI 和 winget、Scoop 等软件源。等有了正式发布再看。
- 只为当前用户安装（不要管理员权限）的选项。那样加不了防火墙规则，也装不了以后的服务。
