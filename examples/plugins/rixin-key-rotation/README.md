# 日日新 Key 轮换示例插件

前端 API URL 保持 `https://token.sensenova.cn/v1/responses`，支持单个 API Key 或按配置顺序轮换多个 Key。

- 需要支持 `request.lifecycle.api_key` 的 Codey 宿主，适用于宿主原生 Responses 线路。
- 导入后默认停用；填写 `apiKey` 或 `apiKeys` 后再启用，两种字段只能保留一种，默认空 Key 配置会拒绝启用。
- 每个新请求按顺序选取一次 Key，同一请求重试复用原 Key；重新启用或重启后从第一项开始。
- 线路 API Key 字段可填非空占位符，匹配的请求发送前由宿主使用插件选定的真实 Key。

Key 保存在用户的插件配置中，请妥善保护。仅加载可信来源的原生插件，它拥有宿主进程权限。开发接口与打包方式见仓库的 Codey Plugin SDK 文档。
