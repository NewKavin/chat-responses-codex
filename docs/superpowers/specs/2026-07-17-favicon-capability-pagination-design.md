# 清新页签图标与 Capability 分页设计

## 背景

当前浏览器页签使用 `frontend/public/favicon.svg`。图标是蓝黑渐变底色与 `CRC` 文本，
与新版控制台统一使用的青绿色强调色和单字母 `C` 品牌标记不一致；小尺寸页签中三个字母也不够清楚。

排障中心的 Capability 策略区域直接把全部 dialect profiles 传给 Element Plus 表格。
当 profiles 数量增长时，页面一次渲染全部行，浏览和定位都不方便。

## 目标

- 浏览器页签图标与新版控制台品牌一致，并在 16x16 像素下保持清楚。
- Capability 策略主 Profiles 表使用前端分页，默认每页 10 条。
- 用户可切换每页 10、20 或 50 条。
- 分页器在桌面和移动端均不扩大页面文档宽度。
- 所有资源继续完全本地加载，内网无公网时可正常使用。

## 非目标

- 不新增 Web App Manifest、apple-touch-icon、PNG 或 ICO 资源。
- 不修改 Capability API、后端排序、存储或数据结构。
- 不分页单个 profile 的 resolved capabilities 和 conflicts 详情表。
- 不包含 Vite、路由、轮询或其他页面性能优化。
- 不修改任何 Rust 或后端文件。

## 页签图标

继续使用现有 `/favicon.svg` 引用和 64x64 viewBox，只替换 SVG 内容：

- 背景使用新版浅色主题强调色 `#0f8f76`；
- 使用适度圆角的正方形轮廓，与 AppShell 和 AuthShell 的品牌标记一致；
- 中心使用白色几何 `C` path，不依赖 Arial 等系统字体；
- 不使用渐变、阴影、小字或细线，避免小尺寸栅格化后发糊；
- SVG 不引用外部图片、字体、样式表或脚本。

`frontend/index.html` 保留现有 `image/svg+xml` favicon 声明，因此无需增加入口逻辑。

## Capability 分页

分页只作用于 Capability 策略区域的主 `dialectProfiles` 表。

组件维护两个本地状态：

- 当前页，初始值为 1；
- 每页条数，初始值为 10，可选值为 10、20、50。

表格数据通过 computed 从 `dialectProfiles` 切片得到。API 仍一次返回完整列表，保留后端
`BTreeMap` 已提供的稳定顺序，不在前端重新排序。

分页器位于主 Profiles 表格下方，显示总数、每页条数、上一页、页码和下一页。切换每页条数时
回到第一页；刷新 Profiles 后，如果当前页超过新的最大页数，则自动校正到最后一个有效页，
空列表时保持第一页。

resolved capabilities 和 conflicts 表是所选 profile 的详情，继续完整显示，不共享主表页码。

## 响应式与可访问性

分页器在桌面右对齐。内容区较窄时，其外层允许自身横向滚动并左对齐，避免撑宽页面。
Element Plus 分页按钮保留原生键盘操作和禁用状态，不增加自定义点击语义。

## 测试与验收

自动化契约覆盖：

- favicon 仍是本地 SVG，包含新版青绿色背景和几何 path；
- favicon 不包含渐变、`text`、外链、脚本或字体依赖；
- Profiles 表绑定分页后的 computed 数据；
- 默认每页 10 条，可选 10、20、50；
- 分页总数来自完整 `dialectProfiles.length`；
- 切换每页条数和刷新数据时具有页码校正逻辑；
- 详情表仍绑定原有完整数据。

浏览器验收覆盖页签 16x16/常规缩放显示，以及 Capability 数据超过 10 条时的翻页、条数切换、
刷新和移动端横向溢出检查。
