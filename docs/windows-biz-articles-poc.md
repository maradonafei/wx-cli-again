# Windows 公众号文章 POC

本项目基线来自公开镜像 [`jackwener/wx-cli-again`](https://github.com/jackwener/wx-cli-again) 的提交
[`077a54c`](https://github.com/jackwener/wx-cli-again/commit/077a54cbfe679bda963cd038d8440422907fc797)。

公开镜像当前没有 Release；其 README 中的预编译下载入口指向私有的
[`botiverse/wx-cli` Releases](https://github.com/botiverse/wx-cli/releases)，无读取权限时会返回 404。本项目改用
GitHub Actions 的 Windows 托管运行器从同一份源码构建 `wx-windows-x86_64.exe`，避免依赖公开 npm 的旧版
0.3.0，也不要求本机安装 Visual Studio。

## 1. 把源码放到自己的 GitHub 仓库

将本目录提交并推送到你有写权限的 GitHub 仓库。工作流文件必须位于默认分支，GitHub 才会显示手动运行按钮：

```text
.github/workflows/build-windows.yml
```

个人构建仓库只保留这个工作流；上游用于正式发布的高权限、多平台 `release.yml` 已移除。当前工作流只有
`contents: read` 权限，不创建 Release、不发布 npm，也不需要配置密钥。它在 GitHub 的 `windows-latest`
托管运行器上执行锁定依赖的 release 构建，并验证输出版本为 `0.6.3`。

官方依据：

- [手动运行工作流](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)
- [保存和下载工作流构件](https://docs.github.com/en/actions/tutorials/store-and-share-data)
- [`dtolnay/rust-toolchain` 的 targets 参数](https://github.com/dtolnay/rust-toolchain#inputs)

## 2. 云端构建并下载

在 GitHub 仓库页面依次进入：

```text
Actions → Build Windows binary → Run workflow → Run workflow
```

运行成功后，打开该次运行，在 `Artifacts` 区域下载 `wx-windows-x86_64`。浏览器通常会得到：

```text
wx-windows-x86_64.zip
```

把 zip 原样放到项目根目录即可：

```text
D:\AI TASK\common_tools\wx-cli\wx-windows-x86_64.zip
```

POC 脚本会安全提取其中唯一的 `wx-windows-x86_64.exe`。也可手工解压后只放 exe。zip、exe 和项目本地
`.local` 目录均不应提交到仓库。

## 3. 运行 POC 入口

普通 PowerShell：

```powershell
Set-Location 'D:\AI TASK\common_tools\wx-cli'
& '.\scripts\Invoke-WxBizArticlesPoc.ps1'
```

脚本会：

1. 安装到项目本地 `.local\bin\wx.exe`，不修改系统 PATH；
2. 验证版本为 `0.6.3` 并报告 SHA-256；
3. 运行 `wx doctor --json`；
4. 前置条件通过后运行 `wx biz-articles -n 200 --json`；
5. 只输出数量、字段覆盖率和 URL 域名检查，不输出公众号名称、标题、URL、数据库路径或密钥。

## 4. 首次初始化边界

如果脚本返回 `needs_init_or_keys`，保持微信 4.x 已登录，然后在管理员 PowerShell 中执行：

```powershell
& 'D:\AI TASK\common_tools\wx-cli\.local\bin\wx.exe' init
```

初始化会读取本机微信进程并把数据库密钥写入用户目录。密钥不得复制到项目、日志或聊天中。完成后回到普通
PowerShell，重新运行 POC 脚本。

## 验收边界

本阶段只验证“本机公众号推送发现能力”，不抓取文章正文、不调用 LLM、不建立生产队列。通过标准：

- `biz-articles` 能成功返回数据；
- 文章字段覆盖率可计算；
- URL 均属于 `mp.weixin.qq.com`；
- 后续人工抽查 3～5 个公众号，确认多图文展开和接收时间符合微信客户端；
- 查询失败时能明确区分未初始化、缺密钥和缺 `biz_message_*.db` 分片。
