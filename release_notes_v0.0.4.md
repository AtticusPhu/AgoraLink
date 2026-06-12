AgoraLink v0.0.4

主要更新：

- 优化聊天主界面布局，去除多余标题和重复入口。
- 登录聊天后隐藏“进入聊天”按钮。
- 清理联系人列表上方可能泄露的内部选择值，例如 direct::peer::name。
- 一对一聊天默认隐藏详情栏，左侧联系人高亮作为当前会话提示。
- 群聊自动显示详情栏，用于查看群成员、成员数量、加成员、退群，以及群主移除成员。
- 群聊消息中显示发言人昵称，便于区分不同成员发言。
- 文件选择器改进为更接近 7-Zip 文件管理器的详细列表样式。
- 文件选择器支持文件和文件夹混选。
- 多选项目默认合并为 ZIP 发送，可在设置中关闭。
- 文件选择器增加图片“预览”按钮，避免列表自动加载大图导致卡顿。
- 文件选择器表头文字上下、左右居中。
- 文件卡片宽度增加，长文件名和传输状态显示更清晰。
- 保留 ZIP_STORED 打包策略：只打包，不压缩。
- 保留局域网聊天、文件发送、接收、断点续传和传输状态显示功能。

使用方式：

- 安装版：运行 AgoraLink_Setup_v0.0.4.exe。
- 免安装版：解压 AgoraLink_portable_v0.0.4.zip 后运行 AgoraLink.exe。
- 首次运行时，请允许 Windows 防火墙专用网络访问。
- 两台设备需要处于同一局域网内，或网络允许 UDP 通信。

发布打包：

- 在项目根目录运行：
  `powershell -ExecutionPolicy Bypass -File scripts/package_release_v0_0_4.ps1`
- 脚本会依次执行 py_compile、PyInstaller、NSIS、免安装 ZIP 打包，并输出两个发布文件的 SHA256。
- 发布产物：
  - dist/AgoraLink_Setup_v0.0.4.exe
  - dist/AgoraLink_portable_v0.0.4.zip
- build/、dist/、__pycache__/、*.exe、*.zip、*.db、*.key、*.pin 不提交到仓库。

GitHub Release 说明：

- Tag：v0.0.4
- Release title：AgoraLink v0.0.4
- 上传附件：
  - AgoraLink_Setup_v0.0.4.exe
  - AgoraLink_portable_v0.0.4.zip
- Release 正文应包含本文件“主要更新”“使用方式”和脚本输出的 SHA256。
- 回滚方式：卸载当前安装版后安装上一版本，或删除免安装目录后解压上一版本 portable 包。回滚前建议备份本机数据目录 `%LOCALAPPDATA%\AgoraLink`。

基础回归测试清单：

- py_compile 检查通过。
- PyInstaller 生成 dist/AgoraLink/AgoraLink.exe。
- NSIS 生成 dist/AgoraLink_Setup_v0.0.4.exe。
- 免安装 ZIP 可解压，解压后 AgoraLink.exe 可启动。
- 首次启动后可解锁聊天数据库并自动启动接收端。
- 两台局域网设备可发现对方并完成联系人请求/接受。
- 一对一文本消息可发送、接收，并显示送达/已读状态。
- 文件可发送、接收，接收目录中能看到完整文件。
- 中断后重新发送同一文件时，断点续传路径可正常处理。
- 群聊可创建、添加成员、查看群详情、发送群消息、移除成员或退群。
- 文件选择器可选择单文件、多文件、文件夹，默认多选打包为 ZIP_STORED。
