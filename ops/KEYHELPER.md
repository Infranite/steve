# steve-keyhelper：保险箱主密钥的钥匙串访问

## 为什么要有它
- 登录钥匙串里，一个条目会记住"是哪个程序创建的"。
- 没有苹果 Team ID 的程序会被记成 `partition_id = cdhash:<创建者的指纹>`。每次重新编译 Hub，指纹都会变，新 Hub 就读不到旧 Hub 创建的条目。
- 2026-11-02 在登录钥匙串里实测过：同一个程序可以重复读取；用同一证书、同一标识签名、只是重新编译过的程序，读取时返回 -25293。
- 临时建的钥匙串文件不加分区，所以在那里测不出这个问题。

## 做法
- 只有 `~/.steve/bin/steve-keyhelper` 一个程序直接读写钥匙串，源码在 `hub-rs/crates/steve-keyhelper`。
- 它放在各次发布的目录之外，只在它自己的源码改动时才重新编译，所以它的指纹，以及它所创建条目的分区，在 Hub 多次发布之间保持不变。
- 它只接受一种调用方：父进程签名为 `io.github.infranite.steve.hub`，并且使用和它自己相同的证书。
- Hub 通过 `HelperStore`（`steve-modules/src/vault/keystore/helper.rs`）直接调用它，中间不经过 shell。密钥只经管道传递。

## 发布（`ops/release.sh` 的 `keyhelper_install`）
- 计算它的源码哈希，和 `~/.steve/bin/steve-keyhelper.srchash` 比较；没有变化就跳过。
- 有变化时：
  1. 编译新版本，签名为 `io.github.infranite.steve.keyhelper`；
  2. 执行 `steve-keyhelper.new migrate-from ~/.steve/bin/steve-keyhelper`：旧版本读出每个条目，用 `/usr/bin/security` 删除旧条目，新版本重新写入；
  3. 迁移成功后再替换旧版本。
- 迁移失败时，已经改动的条目会交回旧版本，发布中止。

## 现有部署的身份兼容

公开源码使用 `io.github.infranite.steve.*` 作为新安装默认值。已有部署必须在构建和发布环境明确保留原来的三个值：

- `STEVE_KEYHELPER_SERVICE`：原钥匙串 service；Hub 与 helper 编译时必须相同。
- `STEVE_KEYHELPER_HUB_IDENTIFIER`：原 Hub 签名 identifier。
- `STEVE_KEYHELPER_IDENTIFIER`：原 helper 签名 identifier。
- `STEVE_LABEL`：原 launchd job label，重启仍指向原服务。

使用原签名证书和 signing keychain。上述配置为编译时身份，不能只写到运行时 `steve.env`。发布脚本会把身份纳入 helper 的构建哈希。

首次启用或改变身份之前，先备份数据并验证恢复码；不要删除旧钥匙串条目来尝试修复读取失败。跨 service 的迁移不由此脚本自动执行，应在隔离副本上验证后单独安排。本次源码清理没有访问或迁移任何实际密钥。

## 测试
- `cargo test -p steve-keyhelper --test keychain -- --ignored --nocapture`：使用临时钥匙串和临时证书。
- 这个测试能验证两件事：
  - 重新编译的 Hub 能读到；
  - 拒绝不该放行的调用方。
- 分区本身只能在登录钥匙串里验证。

## GitHub 预编译包

预编译包使用发行方的构建身份，不能假定能够访问现有部署的私有签名 ACL。已有 macOS 保险箱应继续使用原签名证书和原 build identity 自行构建；切换到预编译包之前单独验证密钥恢复流程。升级管理器不得把下载二进制视为已完成钥匙串迁移。
