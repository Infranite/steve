Homebrew 安装时，如果 GitHub 资产 API 下载失败，会自动尝试同一版本的官方 Release 下载地址。两条路径使用相同的 SHA256 校验，不需要提供 GitHub 凭据或在本机编译。

安装说明补充了 Linuxbrew 自身可能安装的编译器和 libc 基础依赖；Steve 仍从已发布的二进制包安装，原生 CLI 无需 Python 或 OpenSSL。
