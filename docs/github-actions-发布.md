# GitHub Actions 自动化发布说明

本项目新增了 `.github/workflows/AudioLink-release.yml`，用于自动校验 Flutter Android 客户端，并构建 Android 客户端和 Rust Windows 桌面端。发布标签触发时会创建包含两端产物的 GitHub Release。

## 触发规则

- 推送到 `main` 或 `master`：执行 `flutter pub get`、`flutter analyze`、`flutter test`，然后构建签名 Android release APK 和 Rust Windows EXE，自动发布到 GitHub Release，并把产物作为 Actions Artifact 保存 30 天。
- 创建并推送 `v*` 形式的标签，例如 `v1.0.0`：在校验通过后构建 Android release APK 和 Rust Windows EXE，并一起上传到 GitHub Release。
- 手动运行 workflow：可以填写 `tag_name`。填写标签时会用当前选择的分支构建，并创建或更新对应 GitHub Release；如果标签不存在，发布步骤会自动创建这个标签；留空时会使用 `Flutter/pubspec.yaml` 的版本号自动发布。

## 需要配置的仓库 Secret

进入 GitHub 仓库的 `Settings` -> `Secrets and variables` -> `Actions`，添加以下 Repository secrets：

- `ANDROID_KEYSTORE_BASE64`：Android 签名 keystore 文件的 Base64 内容。
- `ANDROID_KEY_ALIAS`：签名 key alias。
- `ANDROID_KEY_PASSWORD`：签名 key 密码。
- `ANDROID_STORE_PASSWORD`：keystore 密码。

在 Windows PowerShell 中，可以用下面的命令把 keystore 转成 Base64 并复制到剪贴板：

```powershell
[Convert]::ToBase64String([IO.File]::ReadAllBytes("Flutter\android\app\release-keystore.jks")) | Set-Clipboard
```

如果你的 keystore 文件不在 `Flutter\android\app\release-keystore.jks`，把命令里的路径换成实际路径即可。

## 版本号规则

标签发布时，workflow 会从标签名推导 Android `versionName`：

- `v1.0.0` -> `versionName=1.0.0`
- `v1.2.3` -> `versionName=1.2.3`

GitHub Release 的标题会保留标签名前缀，例如 `v1.0.0` 会显示为 `Audio Link v1.0.0`。触发标签本身仍应使用 `v1.0.0` 这种格式。手动运行时如果仓库里还没有这个标签，发布步骤会把标签创建到本次 workflow 运行对应的提交上。

Android `versionCode` 使用 GitHub Actions 的运行编号 `github.run_number`，保证每次发布都会递增。

没有手动填写 `tag_name`、也不是 `v*` 标签触发时，workflow 会从 `Flutter/pubspec.yaml` 读取版本号并自动补上 `v` 前缀。例如 `version: 1.0.0+1` 会发布到 `v1.0.0`。

## 发布产物

发布流程会生成并上传：

- `AudioLink-Android-版本号.apk`
- `AudioLink-Windows-版本号.exe`

APK 是 Android 客户端安装包。Windows EXE 是 Rust 桌面端程序。

注意：GitHub Actions 的 Artifacts 下载入口会由 GitHub 自动打包成 zip，这是平台行为；GitHub Release 页面上传的是独立的 APK 和 EXE。

## 注意事项

- release 构建依赖签名 Secret；如果缺少 Secret，workflow 会在签名准备阶段失败并明确提示缺失项。
- workflow 会在 CI 运行时临时生成 `Flutter/android/key.properties` 和 keystore 文件，不会把签名文件写入仓库。
- 当前方案发布到 GitHub Release，不会自动提交到 Google Play。需要商店发布时，可以在现有 Android release job 后继续追加 AAB 或 Google Play 发布步骤。
