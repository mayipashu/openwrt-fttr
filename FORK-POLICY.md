# 本 fork 的分支策略

**本仓库不再同步上游。** 从 2026-11-28 起，`mayipashu/openwrt-fttr` 是
H3C HM2004-DU 的**独立维护分支**，只用来承载自己的修改，不再 merge / rebase
上游 `LazuliKao/openwrt-fttr`。

## 为什么冻结

冻结时的基点是上游 `e78b5a3`（2026-11-27）。上游 `main` 之后继续演进，
但**我们修过的两个光口问题它始终没修**，代码仍是原样：

1. `fttr/src/src/platform/h3c_hm2004_du/fmcs.rs` → `update_gpon_control()`
   对本函数不拥有的位做严格读回比较。FMCS FPGA 在下行 PON 引擎运行期间
   会自置该寄存器 `0x01000004` 的其它位，于是出现

   ```
   GPON control readback mismatch: wanted 0x00000002, got 0x00000003
   ```

   调用方据此 `fail_closed()`：关发射、状态钉死在 `OpticalState::Unknown`。

2. `fttr/src/src/controller/optical_owner.rs` → `reconcile()`
   把**周期性轮询**（每秒一次）的失败也送进 `fail_closed()`。
   任何一次 SPI 抖动都会关掉下行光，而 `Unknown` 没有自动恢复路径，
   只能人工 `fttrctl optical reload`。表现是开机一段时间后 FTTR 从机全掉。

跟着上游走 = 反复把这两个坑带回来，所以改为冻结。

## 分支

| 分支 | 用途 |
|---|---|
| `hm2004-stable` | **默认分支，构建只用这个。** 上游 `e78b5a3` + 两个光口修复 |
| `main` | 与 `hm2004-stable` 指向同一提交，仅作兜底，避免误用未修复代码 |
| `hm2004-stable-backup-1128` | 旧基线（上游 `c6192f6` + 两个修复）留档 |

## 改代码的规则

1. 直接在 `hm2004-stable` 上提交，不建立上游跟踪分支。
2. **不要**使用 GitHub 的 “Sync fork”，也不要 `git merge upstream/main`。
3. 改了 `fmcs.rs` 或 `optical_owner.rs` 的话，必须保证下面三个标记还在，
   否则固件构建会在 `diy-part1.sh` 的特征断言处 `::error` 停下：

   - `GPON_CONTROL_READBACK_ATTEMPTS`（写后有界重试）
   - `let owned = GPON_TX | GPON_FEC`（只比较自有位）
   - `a_poll_failure_keeps_the_line_up`（轮询失败不关光口的测试名）

## 第三方依赖

`rumqttd` 原本指向 `github.com/LazuliKao/rumqtt.git`，已改为自己的 fork
`github.com/mayipashu/rumqtt.git`，**rev 不变**（`e7ad1ac9`，即其 `main` 尖端），
代码完全等价，只是不再依赖上游仓库的可用性。
`Cargo.lock` 的 `source` 字段已同步，`cargo metadata --locked` 通过。

## 固件构建怎么取

构建仓库 `mayipashu/Actions-PONWRT-HM2004` 的默认值：

```
PRIVATE_REPO        = mayipashu/openwrt-fttr
PRIVATE_REPO_BRANCH = hm2004-stable
FTTR_LUCI_REPO      = mayipashu/luci-app-fttr   (branch: package)
```

`luci-theme-fluent` 同样已 fork 到 `mayipashu/luci-theme-fluent`，
构建脚本从该 fork 克隆。至此整条构建链路不再访问 `LazuliKao/*`。
